//! Local embedding and reranking models for the Memory memory daemon (SPEC.md §3).
//!
//! * [`Embedder`]: BAAI/bge-m3 (XLM-RoBERTa large, 24 layers, 1024 hidden), CLS pooling,
//!   L2-normalised 1024-d dense vectors.
//! * [`Reranker`]: BAAI/bge-reranker-v2-m3 (or bge-reranker-base), an
//!   `XLMRobertaForSequenceClassification` cross-encoder with one label; scores are
//!   `sigmoid(logit)`.
//!
//! CPU by default, optional Metal GPU, f32. Inputs are tokenised with `tokenizer.json`, truncated
//! to 512 tokens, sorted by length and run in padded batches bounded by a token budget. `load`
//! preserves portable CPU inference; `load_on` shares a device selected by `select_device`.
//! The Metal path keeps the word table, encoder/classifier weights and all intermediate arithmetic
//! on GPU, including CLS L2-normalisation and sigmoid. Only final outputs return to CPU.

use std::path::Path;

use anyhow::{bail, Context, Result};
use candle_core::{Device, Tensor};
use tokenizers::{
    EncodeInput, Tokenizer, TruncationDirection, TruncationParams, TruncationStrategy,
};

mod device;
pub mod kernels;
mod tensor_encoder;
mod weights;
mod xlmr;

pub use device::{runtime_backend, runtime_device, select_device, DeviceChoice};
pub use weights::{convert_pth_to_safetensors, ensure_safetensors};
pub use xlmr::XlmrConfig;

use weights::Source;
use xlmr::Encoder;

/// Max tokens per sequence (embedding text, or query+document pair), special tokens included.
pub const MAX_LEN: usize = 512;
/// Default bound on padded tokens per forward pass (memory: roughly 80-100 KB per padded token
/// for XLM-R large at length 512, much less for short inputs).
pub const DEFAULT_TOKEN_BUDGET: usize = 4096;
/// Default bound on sequences per forward pass.
pub const DEFAULT_MAX_BATCH: usize = 64;

/// Batching limits.
#[derive(Debug, Clone, Copy)]
pub struct BatchLimits {
    /// Max `batch_size * padded_length` per forward pass.
    pub token_budget: usize,
    /// Max sequences per forward pass.
    pub max_batch: usize,
}

impl Default for BatchLimits {
    fn default() -> Self {
        Self {
            token_budget: DEFAULT_TOKEN_BUDGET,
            max_batch: DEFAULT_MAX_BATCH,
        }
    }
}

/// Split sequence indices into batches: sort by length (descending), then greedily fill each
/// batch while `count * longest <= token_budget` and `count <= max_batch`.
fn plan_batches(lens: &[usize], limits: BatchLimits) -> Vec<Vec<usize>> {
    let mut order: Vec<usize> = (0..lens.len()).collect();
    order.sort_by(|&a, &b| lens[b].cmp(&lens[a]).then(a.cmp(&b)));
    let mut out = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    let mut longest = 0usize;
    for i in order {
        let l = lens[i].max(1);
        let new_longest = longest.max(l);
        if !cur.is_empty()
            && ((cur.len() + 1) * new_longest > limits.token_budget.max(1)
                || cur.len() + 1 > limits.max_batch.max(1))
        {
            out.push(std::mem::take(&mut cur));
            longest = 0;
        }
        longest = longest.max(l);
        cur.push(i);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn load_tokenizer(dir: &Path, max_len: usize) -> Result<Tokenizer> {
    let p = dir.join("tokenizer.json");
    let mut tok =
        Tokenizer::from_file(&p).map_err(|e| anyhow::anyhow!("load {}: {e}", p.display()))?;
    tok.with_padding(None);
    tok.with_truncation(Some(TruncationParams {
        max_length: max_len,
        strategy: TruncationStrategy::LongestFirst,
        stride: 0,
        direction: TruncationDirection::Right,
    }))
    .map_err(|e| anyhow::anyhow!("tokenizer truncation: {e}"))?;
    Ok(tok)
}

/// "BAAI/<dir name>" for bge-* directories, otherwise the directory name.
fn model_id_from_dir(dir: &Path) -> String {
    let name = dir
        .canonicalize()
        .ok()
        .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()))
        .or_else(|| dir.file_name().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "unknown".to_string());
    if name.starts_with("bge-") {
        format!("BAAI/{name}")
    } else {
        name
    }
}

fn encodings_to_ids(encs: Vec<tokenizers::Encoding>) -> Vec<Vec<u32>> {
    encs.into_iter().map(|e| e.get_ids().to_vec()).collect()
}

/// Opens the weights of `dir`, converting `pytorch_model.bin` to `model.safetensors` once if
/// needed (falls back to reading the `.bin` directly if the conversion fails).
fn open_weights(dir: &Path) -> Result<Source> {
    if !dir.join(weights::SAFETENSORS_NAME).is_file() && dir.join(weights::PTH_NAME).is_file() {
        let t0 = std::time::Instant::now();
        match ensure_safetensors(dir) {
            Ok(_) => tracing::info!(
                "converted {}/{} to {} in {:.1}s",
                dir.display(),
                weights::PTH_NAME,
                weights::SAFETENSORS_NAME,
                t0.elapsed().as_secs_f64()
            ),
            Err(e) => tracing::warn!(
                "could not convert {} to safetensors ({e:#}); loading the .bin directly",
                dir.display()
            ),
        }
    }
    Source::open(dir)
}

// =============================================================================================

/// bge-m3 dense embedder (CLS pooling, L2-normalised).
pub struct Embedder {
    enc: Encoder,
    tok: Tokenizer,
    model_id: String,
    limits: BatchLimits,
}

impl Embedder {
    /// Load from a directory containing `config.json`, `tokenizer.json` and
    /// `model.safetensors` (or `pytorch_model.bin`, which is converted once).
    pub fn load(dir: &Path) -> Result<Self> {
        Self::load_on(dir, &Device::Cpu)
    }

    pub fn load_on(dir: &Path, device: &Device) -> Result<Self> {
        let cfg = XlmrConfig::load(dir)?;
        let enc = {
            let src = open_weights(dir)?;
            let prefix = xlmr::detect_prefix(&src)?;
            Encoder::load_on(cfg, &src, &prefix, MAX_LEN, device)
                .with_context(|| format!("loading encoder from {}", dir.display()))?
        };
        let tok = load_tokenizer(dir, enc.max_len)?;
        Ok(Self {
            enc,
            tok,
            model_id: model_id_from_dir(dir),
            limits: BatchLimits::default(),
        })
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    pub fn device(&self) -> &Device {
        self.enc.device()
    }
    pub fn backend(&self) -> &'static str {
        runtime_backend(self.device())
    }

    pub fn dim(&self) -> usize {
        self.enc.hidden_size()
    }

    pub fn limits(&self) -> BatchLimits {
        self.limits
    }

    pub fn set_limits(&mut self, limits: BatchLimits) {
        self.limits = limits;
    }

    /// Token ids (with special tokens, truncated) as the model sees them.
    pub fn tokenize(&self, texts: &[String]) -> Result<Vec<Vec<u32>>> {
        let inputs: Vec<EncodeInput> = texts.iter().map(|t| t.as_str().into()).collect();
        let encs = self
            .tok
            .encode_batch(inputs, true)
            .map_err(|e| anyhow::anyhow!("tokenize: {e}"))?;
        Ok(encodings_to_ids(encs))
    }

    /// Embed texts; returns one L2-normalised vector per text, in input order.
    pub fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let ids = self.tokenize(texts)?;
        self.embed_ids(&ids)
    }

    /// Embed pre-tokenised sequences (see [`Embedder::tokenize`]).
    pub fn embed_ids(&self, ids: &[Vec<u32>]) -> Result<Vec<Vec<f32>>> {
        let h = self.dim();
        let lens: Vec<usize> = ids.iter().map(|s| s.len()).collect();
        let mut out: Vec<Vec<f32>> = vec![Vec::new(); ids.len()];
        for batch in plan_batches(&lens, self.limits) {
            let seqs: Vec<&[u32]> = batch.iter().map(|&i| ids[i].as_slice()).collect();
            let cls = self.enc.forward_cls(&seqs)?; // (b, H)
            let cls = if self.device().is_metal() {
                let norm = cls
                    .sqr()?
                    .sum_keepdim(1)?
                    .sqrt()?
                    .clamp(1e-12f32, f32::MAX)?;
                cls.broadcast_div(&norm)?
            } else {
                cls
            };
            let cls = cls
                .to_device(&Device::Cpu)?
                .flatten_all()?
                .to_vec1::<f32>()?;
            for (k, &i) in batch.iter().enumerate() {
                let mut v = cls[k * h..(k + 1) * h].to_vec();
                if self.device().is_cpu() {
                    let norm = v
                        .iter()
                        .map(|x| (*x as f64) * (*x as f64))
                        .sum::<f64>()
                        .sqrt();
                    let inv = if norm > 0.0 { (1.0 / norm) as f32 } else { 0.0 };
                    v.iter_mut().for_each(|x| *x *= inv);
                }
                out[i] = v;
            }
        }
        Ok(out)
    }
}

// =============================================================================================

/// Cross-encoder reranker (`XLMRobertaForSequenceClassification`, 1 label).
pub struct Reranker {
    enc: Encoder,
    tok: Tokenizer,
    model_id: String,
    limits: BatchLimits,
    dense_w: Tensor, // (H, H)
    dense_b: Vec<f32>,
    out_w: Vec<f32>, // (H)
    out_b: f32,
    device_head: Option<(Tensor, Tensor, Tensor)>, // dense bias, output weight and output bias on GPU
}

impl Reranker {
    pub fn load(dir: &Path) -> Result<Self> {
        Self::load_on(dir, &Device::Cpu)
    }

    pub fn load_on(dir: &Path, device: &Device) -> Result<Self> {
        let cfg = XlmrConfig::load(dir)?;
        if cfg.num_labels() != 1 {
            bail!(
                "reranker must have exactly one label, config has {}",
                cfg.num_labels()
            );
        }
        let (enc, dense_w, dense_b, out_w, out_b) = {
            let src = open_weights(dir)?;
            let prefix = xlmr::detect_prefix(&src)?;
            let h = cfg.hidden_size;
            let dense_w = src.get("classifier.dense.weight")?.to_device(device)?;
            let dense_b = src.get_vec("classifier.dense.bias")?;
            let out_w = src.get_vec("classifier.out_proj.weight")?;
            let out_b = src.get_vec("classifier.out_proj.bias")?;
            if dense_w.dims() != [h, h]
                || dense_b.len() != h
                || out_w.len() != h
                || out_b.len() != 1
            {
                bail!("unexpected classifier head shapes");
            }
            let enc = Encoder::load_on(cfg, &src, &prefix, MAX_LEN, device)
                .with_context(|| format!("loading encoder from {}", dir.display()))?;
            (enc, dense_w, dense_b, out_w, out_b[0])
        };
        let tok = load_tokenizer(dir, enc.max_len)?;
        let device_head = if device.is_metal() {
            Some((
                Tensor::from_vec(dense_b.clone(), dense_b.len(), device)?,
                Tensor::from_vec(out_w.clone(), (1, out_w.len()), device)?,
                Tensor::from_vec(vec![out_b], (1, 1), device)?,
            ))
        } else {
            None
        };
        Ok(Self {
            enc,
            tok,
            model_id: model_id_from_dir(dir),
            limits: BatchLimits::default(),
            dense_w,
            dense_b,
            out_w,
            out_b,
            device_head,
        })
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    pub fn device(&self) -> &Device {
        self.enc.device()
    }
    pub fn backend(&self) -> &'static str {
        runtime_backend(self.device())
    }

    pub fn limits(&self) -> BatchLimits {
        self.limits
    }

    pub fn set_limits(&mut self, limits: BatchLimits) {
        self.limits = limits;
    }

    /// Token ids of `<s> query </s></s> doc </s>` pairs (truncated longest-first to 512).
    pub fn tokenize_pairs(&self, query: &str, docs: &[String]) -> Result<Vec<Vec<u32>>> {
        let inputs: Vec<EncodeInput> = docs
            .iter()
            .map(|d| (query.to_string(), d.clone()).into())
            .collect();
        let encs = self
            .tok
            .encode_batch(inputs, true)
            .map_err(|e| anyhow::anyhow!("tokenize: {e}"))?;
        Ok(encodings_to_ids(encs))
    }

    /// Raw logits (same order as `docs`).
    pub fn logits(&self, query: &str, docs: &[String]) -> Result<Vec<f32>> {
        if docs.is_empty() {
            return Ok(Vec::new());
        }
        let ids = self.tokenize_pairs(query, docs)?;
        self.logits_ids(&ids)
    }

    /// Raw logits for pre-tokenised pairs.
    pub fn logits_ids(&self, ids: &[Vec<u32>]) -> Result<Vec<f32>> {
        self.output_ids(ids, false)
    }

    fn output_ids(&self, ids: &[Vec<u32>], scores: bool) -> Result<Vec<f32>> {
        let h = self.enc.hidden_size();
        let lens: Vec<usize> = ids.iter().map(|s| s.len()).collect();
        let mut out = vec![0f32; ids.len()];
        for batch in plan_batches(&lens, self.limits) {
            let seqs: Vec<&[u32]> = batch.iter().map(|&i| ids[i].as_slice()).collect();
            let cls = self.enc.forward_cls(&seqs)?; // (b, H)
            let z = cls.matmul(&self.dense_w.t()?)?; // (b, H)
            if let Some((dense_b, out_w, out_b)) = &self.device_head {
                let logits = z
                    .broadcast_add(dense_b)?
                    .tanh()?
                    .matmul(&out_w.t()?)?
                    .broadcast_add(out_b)?
                    .flatten_all()?;
                let final_values = if scores {
                    candle_nn::ops::sigmoid(&logits)?
                } else {
                    logits
                };
                let values = final_values.to_device(&Device::Cpu)?.to_vec1::<f32>()?;
                for (k, &i) in batch.iter().enumerate() {
                    out[i] = values[k];
                }
                continue;
            }
            let z = z.flatten_all()?.to_vec1::<f32>()?;
            for (k, &i) in batch.iter().enumerate() {
                let row = &z[k * h..(k + 1) * h];
                let mut acc = self.out_b as f64;
                for ((v, b), w) in row.iter().zip(&self.dense_b).zip(&self.out_w) {
                    acc += ((v + b).tanh() as f64) * (*w as f64);
                }
                out[i] = if scores {
                    sigmoid(acc as f32)
                } else {
                    acc as f32
                };
            }
        }
        Ok(out)
    }

    /// Relevance scores `sigmoid(logit)` in 0..1, same order as `docs`.
    pub fn score(&self, query: &str, docs: &[String]) -> Result<Vec<f32>> {
        if docs.is_empty() {
            return Ok(Vec::new());
        }
        self.output_ids(&self.tokenize_pairs(query, docs)?, true)
    }
}

pub fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// Cosine similarity (inputs need not be normalised).
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let (mut d, mut na, mut nb) = (0f64, 0f64, 0f64);
    for (x, y) in a.iter().zip(b) {
        d += (*x as f64) * (*y as f64);
        na += (*x as f64) * (*x as f64);
        nb += (*y as f64) * (*y as f64);
    }
    (d / (na.sqrt() * nb.sqrt()).max(1e-30)) as f32
}

fn proc_status_kb(key: &str) -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    s.lines()
        .find(|l| l.starts_with(key))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse().ok())
}

/// Peak resident set size of this process in MiB (Linux `VmHWM`).
pub fn peak_rss_mib() -> Option<f64> {
    proc_status_kb("VmHWM:").map(|k| k as f64 / 1024.0)
}

/// Current resident set size of this process in MiB (Linux `VmRSS`).
pub fn rss_mib() -> Option<f64> {
    proc_status_kb("VmRSS:").map(|k| k as f64 / 1024.0)
}

#[allow(dead_code)]
fn _assert_send_sync() {
    fn is<T: Send + Sync>() {}
    is::<Embedder>();
    is::<Reranker>();
}

#[allow(dead_code)]
fn _device() -> Device {
    Device::Cpu
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batches_respect_limits() {
        let lens = vec![10, 500, 40, 40, 41, 3, 512, 7];
        let lim = BatchLimits {
            token_budget: 1024,
            max_batch: 3,
        };
        let b = plan_batches(&lens, lim);
        let mut seen: Vec<usize> = b.iter().flatten().copied().collect();
        seen.sort();
        assert_eq!(seen, (0..lens.len()).collect::<Vec<_>>());
        for batch in &b {
            let longest = batch.iter().map(|&i| lens[i]).max().unwrap();
            assert!(batch.len() <= 3);
            assert!(batch.len() == 1 || batch.len() * longest <= 1024);
        }
    }
}
