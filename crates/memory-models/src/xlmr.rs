//! XLM-RoBERTa encoders (inference only, CPU or Metal, f32) with HF semantics:
//! - position ids = padding_idx + cumsum(input_ids != padding_idx), padded tokens get padding_idx;
//! - token type ids are all 0 (type_vocab_size = 1 for XLM-R), folded into the position table;
//! - attention over padded batches ignores padded keys (masked softmax);
//! - GELU (erf) activations, post-LayerNorm blocks.
//!
//! Compared to `candle_transformers::models::xlm_roberta` this implementation
//! - keeps the 1 GB word-embedding table memory-mapped (rows are gathered on demand),
//! - fuses Q/K/V into one matmul and folds the 1/sqrt(d) scale into the query weights,
//! - fuses bias + GELU and bias + residual + LayerNorm into multi-threaded in-place passes,
//! - in the last layer, only computes the CLS row (all callers use CLS pooling / CLS head).

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context, Result};
use candle_core::safetensors::MmapedSafetensors;
use candle_core::{CpuStorage, Device, InplaceOp1, InplaceOp2, Layout, Tensor};
use rayon::prelude::*;

use crate::kernels;
use crate::weights::Source;

#[derive(Debug, Clone, serde::Deserialize)]
pub struct XlmrConfig {
    pub hidden_size: usize,
    pub num_attention_heads: usize,
    pub num_hidden_layers: usize,
    pub intermediate_size: usize,
    pub layer_norm_eps: f64,
    pub pad_token_id: u32,
    pub max_position_embeddings: usize,
    pub vocab_size: usize,
    #[serde(default = "default_type_vocab")]
    pub type_vocab_size: usize,
    #[serde(default = "default_act")]
    pub hidden_act: String,
    #[serde(default)]
    pub position_embedding_type: Option<String>,
    #[serde(default)]
    pub id2label: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub num_labels: Option<usize>,
    #[serde(default)]
    pub model_type: Option<String>,
}

fn default_type_vocab() -> usize {
    1
}
fn default_act() -> String {
    "gelu".to_string()
}

impl XlmrConfig {
    pub fn load(dir: &Path) -> Result<Self> {
        let p = dir.join("config.json");
        let s = std::fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))?;
        let cfg: XlmrConfig =
            serde_json::from_str(&s).with_context(|| format!("parse {}", p.display()))?;
        if cfg.hidden_size == 0
            || cfg.num_attention_heads == 0
            || cfg.num_hidden_layers == 0
            || cfg.intermediate_size == 0
            || cfg.type_vocab_size == 0
            || cfg.pad_token_id as usize >= cfg.vocab_size
        {
            bail!("invalid zero model dimension/layer count or padding token outside vocabulary");
        }
        if !cfg.layer_norm_eps.is_finite() || cfg.layer_norm_eps <= 0.0 {
            bail!("layer_norm_eps must be finite and positive");
        }
        if cfg.hidden_size % cfg.num_attention_heads != 0 {
            bail!("hidden_size not divisible by num_attention_heads");
        }
        if cfg.hidden_act != "gelu" {
            bail!(
                "unsupported hidden_act {:?} (only exact GELU is implemented)",
                cfg.hidden_act
            );
        }
        if let Some(pe) = &cfg.position_embedding_type {
            if pe != "absolute" {
                bail!("unsupported position_embedding_type {pe:?}");
            }
        }
        Ok(cfg)
    }

    pub fn num_labels(&self) -> usize {
        self.num_labels
            .or_else(|| self.id2label.as_ref().map(|m| m.len()))
            .unwrap_or(1)
    }

    /// Maximum number of tokens (incl. special tokens) one sequence may have.
    pub fn max_tokens(&self) -> usize {
        self.max_position_embeddings
            .saturating_sub(self.pad_token_id as usize + 1)
    }
}

enum WordEmb {
    /// Rows are read from the memory-mapped safetensors file (F32, row-major [vocab, hidden]).
    Mapped {
        st: MmapedSafetensors,
        name: String,
    },
    Owned(Vec<f32>),
}

struct Layer {
    wqkv: Tensor, // (3H, H), query rows pre-scaled by 1/sqrt(head_dim)
    bqkv: Vec<f32>,
    wo: Tensor, // (H, H)
    bo: Vec<f32>,
    ln1_g: Vec<f32>,
    ln1_b: Vec<f32>,
    w1: Tensor, // (I, H)
    b1: Vec<f32>,
    w2: Tensor, // (H, I)
    b2: Vec<f32>,
    ln2_g: Vec<f32>,
    ln2_b: Vec<f32>,
}

struct CpuEncoder {
    pub cfg: XlmrConfig,
    h: usize,
    nh: usize,
    hd: usize,
    eps: f32,
    pad: u32,
    /// Max sequence length supported by the kept position rows.
    pub max_len: usize,
    word: WordEmb,
    pos: Vec<f32>, // (rows, H); token-type row 0 folded in
    pos_rows: usize,
    emb_ln_g: Vec<f32>,
    emb_ln_b: Vec<f32>,
    layers: Vec<Layer>,
}

/// Detect the tensor-name prefix ("roberta." for *ForSequenceClassification / *ForMaskedLM
/// checkpoints, "" for bare XLMRobertaModel checkpoints).
pub(crate) fn detect_prefix(src: &Source) -> Result<String> {
    for p in ["", "roberta.", "model.", "xlm_roberta.", "bert."] {
        if src.contains(&format!("{p}embeddings.word_embeddings.weight")) {
            return Ok(p.to_string());
        }
    }
    let mut names = src.names();
    names.sort();
    names.truncate(8);
    bail!("could not find embeddings.word_embeddings.weight; first tensor names: {names:?}")
}

fn get_ln(src: &Source, p: &str) -> Result<(Vec<f32>, Vec<f32>)> {
    let (gw, gb) = (format!("{p}.weight"), format!("{p}.bias"));
    if src.contains(&gw) {
        Ok((src.get_vec(&gw)?, src.get_vec(&gb)?))
    } else {
        Ok((
            src.get_vec(&format!("{p}.gamma"))?,
            src.get_vec(&format!("{p}.beta"))?,
        ))
    }
}

impl CpuEncoder {
    /// Load the encoder. `max_len` caps the sequence length (<= 512 in practice).
    pub fn load(cfg: XlmrConfig, src: &Source, prefix: &str, max_len: usize) -> Result<Self> {
        let h = cfg.hidden_size;
        let nh = cfg.num_attention_heads;
        let hd = h / nh;
        let pad = cfg.pad_token_id;
        let max_len = max_len.min(cfg.max_tokens());
        if max_len < 2 {
            bail!("max_len too small");
        }
        let e = format!("{prefix}embeddings");

        // Word embeddings: keep them mapped if the file is an F32 safetensors file.
        let wname = format!("{e}.word_embeddings.weight");
        let word = match src {
            Source::St(_, path) => {
                // A dedicated mapping that lives as long as the encoder; the loader's mapping is
                // dropped after loading, so encoder weights don't stay double-resident.
                let st = unsafe { MmapedSafetensors::new(path) }?;
                let ok = {
                    let v = st.get(&wname)?;
                    v.dtype() == safetensors::Dtype::F32
                        && v.shape() == [cfg.vocab_size, h]
                        && v.data().len() == cfg.vocab_size * h * 4
                };
                if ok {
                    WordEmb::Mapped {
                        st,
                        name: wname.clone(),
                    }
                } else {
                    WordEmb::Owned(src.get_vec(&wname)?)
                }
            }
            Source::Pth(_) => WordEmb::Owned(src.get_vec(&wname)?),
        };
        if let WordEmb::Owned(v) = &word {
            if v.len() != cfg.vocab_size * h {
                bail!("word embedding size mismatch");
            }
        }

        // Position embeddings (only the rows we can ever use) + token type 0.
        let pos_rows = (pad as usize + 1 + max_len).min(cfg.max_position_embeddings);
        let pos_full = src.get(&format!("{e}.position_embeddings.weight"))?;
        if pos_full.dims() != [cfg.max_position_embeddings, h] {
            bail!("position embedding shape {:?}", pos_full.dims());
        }
        let mut pos = pos_full
            .narrow(0, 0, pos_rows)?
            .flatten_all()?
            .to_vec1::<f32>()?;
        drop(pos_full);
        let tt = src.get_vec(&format!("{e}.token_type_embeddings.weight"))?;
        if tt.len() < h {
            bail!("token type embeddings too small");
        }
        for row in pos.chunks_exact_mut(h) {
            for (x, t) in row.iter_mut().zip(&tt[..h]) {
                *x += t;
            }
        }
        let (emb_ln_g, emb_ln_b) = get_ln(src, &format!("{e}.LayerNorm"))?;
        if emb_ln_g.len() != h || emb_ln_b.len() != h {
            bail!("embedding LayerNorm shape mismatch");
        }

        let scale = 1.0 / (hd as f64).sqrt();
        let mut layers = Vec::with_capacity(cfg.num_hidden_layers);
        for i in 0..cfg.num_hidden_layers {
            let p = format!("{prefix}encoder.layer.{i}");
            let a = format!("{p}.attention.self");
            let wq = (src.get(&format!("{a}.query.weight"))? * scale)?;
            let wk = src.get(&format!("{a}.key.weight"))?;
            let wv = src.get(&format!("{a}.value.weight"))?;
            let wqkv = Tensor::cat(&[&wq, &wk, &wv], 0)?.contiguous()?;
            drop((wq, wk, wv));
            let mut bqkv: Vec<f32> = src
                .get_vec(&format!("{a}.query.bias"))?
                .into_iter()
                .map(|x| (x as f64 * scale) as f32)
                .collect();
            bqkv.extend(src.get_vec(&format!("{a}.key.bias"))?);
            bqkv.extend(src.get_vec(&format!("{a}.value.bias"))?);
            let o = format!("{p}.attention.output");
            let wo = src.get(&format!("{o}.dense.weight"))?;
            let bo = src.get_vec(&format!("{o}.dense.bias"))?;
            let (ln1_g, ln1_b) = get_ln(src, &format!("{o}.LayerNorm"))?;
            let w1 = src.get(&format!("{p}.intermediate.dense.weight"))?;
            let b1 = src.get_vec(&format!("{p}.intermediate.dense.bias"))?;
            let w2 = src.get(&format!("{p}.output.dense.weight"))?;
            let b2 = src.get_vec(&format!("{p}.output.dense.bias"))?;
            let (ln2_g, ln2_b) = get_ln(src, &format!("{p}.output.LayerNorm"))?;
            let inter = cfg.intermediate_size;
            if wqkv.dims() != [3 * h, h]
                || wo.dims() != [h, h]
                || w1.dims() != [inter, h]
                || w2.dims() != [h, inter]
                || bqkv.len() != 3 * h
                || b1.len() != inter
                || bo.len() != h
                || b2.len() != h
                || ln1_g.len() != h
                || ln1_b.len() != h
                || ln2_g.len() != h
                || ln2_b.len() != h
            {
                bail!("unexpected weight shapes in layer {i}");
            }
            layers.push(Layer {
                wqkv,
                bqkv,
                wo,
                bo,
                ln1_g,
                ln1_b,
                w1,
                b1,
                w2,
                b2,
                ln2_g,
                ln2_b,
            });
        }
        Ok(Self {
            eps: cfg.layer_norm_eps as f32,
            cfg,
            h,
            nh,
            hd,
            pad,
            max_len,
            word,
            pos,
            pos_rows,
            emb_ln_g,
            emb_ln_b,
            layers,
        })
    }

    /// Embedding layer for a padded batch: returns (b*t, H) row-major.
    fn embed_tokens(&self, seqs: &[&[u32]], t: usize) -> Result<Vec<f32>> {
        let h = self.h;
        let vocab = self.cfg.vocab_size;
        for s in seqs {
            if s.is_empty() {
                bail!("empty token sequence");
            }
            if s.len() > t || s.len() > self.max_len {
                bail!("sequence longer than max length");
            }
            if let Some(&bad) = s.iter().find(|&&id| id as usize >= vocab) {
                bail!("token id {bad} out of vocabulary");
            }
        }
        let word_bytes: Option<&[u8]> = match &self.word {
            WordEmb::Mapped { st, name } => Some(st.get(name)?.data()),
            WordEmb::Owned(_) => None,
        };
        let word_f32: Option<&[f32]> = match &self.word {
            WordEmb::Owned(v) => Some(v.as_slice()),
            _ => None,
        };
        let pad = self.pad;
        let pos_rows = self.pos_rows;
        let mut buf = vec![0f32; seqs.len() * t * h];
        buf.par_chunks_mut(t * h)
            .enumerate()
            .for_each(|(i, chunk)| {
                let s = seqs[i];
                let mut count = 0u32;
                for j in 0..t {
                    let id = if j < s.len() { s[j] } else { pad };
                    let pid = if id != pad {
                        count += 1;
                        pad + count
                    } else {
                        pad
                    } as usize;
                    let row = &mut chunk[j * h..(j + 1) * h];
                    let off = id as usize * h;
                    if let Some(wb) = word_bytes {
                        let src = &wb[off * 4..(off + h) * 4];
                        // Unaligned-safe little-endian copy.
                        for (x, b) in row.iter_mut().zip(src.chunks_exact(4)) {
                            *x = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                        }
                    } else if let Some(w) = word_f32 {
                        row.copy_from_slice(&w[off..off + h]);
                    }
                    let pid = pid.min(pos_rows - 1);
                    let prow = &self.pos[pid * h..(pid + 1) * h];
                    for (x, p) in row.iter_mut().zip(prow) {
                        *x += p;
                    }
                    kernels::layer_norm_row(row, &self.emb_ln_g, &self.emb_ln_b, self.eps);
                }
            });
        Ok(buf)
    }

    /// Run the encoder on a batch of token-id sequences (each already containing special tokens).
    /// Returns the final hidden state of the first token (CLS / `<s>`) of each sequence: (b, H).
    pub fn forward_cls(&self, seqs: &[&[u32]]) -> Result<Tensor> {
        let b = seqs.len();
        if b == 0 {
            bail!("empty batch");
        }
        let t = seqs.iter().map(|s| s.len()).max().unwrap_or(0);
        if t == 0 {
            bail!("empty sequence");
        }
        let h = self.h;
        let lens: Vec<usize> = seqs.iter().map(|s| s.len()).collect();
        let x = self.embed_tokens(seqs, t)?;
        let mut x = Tensor::from_vec(x, (b * t, h), &candle_core::Device::Cpu)?;
        let n_layers = self.layers.len();
        for (li, l) in self.layers.iter().enumerate() {
            x = self.layer(l, &x, b, t, &lens, li + 1 == n_layers)?;
        }
        Ok(x)
    }

    fn layer(
        &self,
        l: &Layer,
        x: &Tensor,
        b: usize,
        t: usize,
        lens: &[usize],
        cls_only: bool,
    ) -> Result<Tensor> {
        let (h, nh, hd) = (self.h, self.nh, self.hd);
        let qkv = x.matmul(&l.wqkv.t()?)?; // (b*t, 3H)
        qkv.inplace_op1(&BiasAct {
            bias: &l.bqkv,
            gelu: false,
        })?;
        let qkv = qkv.reshape((b, t, 3, nh, hd))?;
        let k = qkv
            .narrow(2, 1, 1)?
            .squeeze(2)?
            .transpose(1, 2)?
            .contiguous()?; // (b,nh,t,hd)
        let v = qkv
            .narrow(2, 2, 1)?
            .squeeze(2)?
            .transpose(1, 2)?
            .contiguous()?;
        let tq = if cls_only { 1 } else { t };
        let q = qkv
            .narrow(1, 0, tq)?
            .narrow(2, 0, 1)?
            .squeeze(2)?
            .transpose(1, 2)?
            .contiguous()?; // (b,nh,tq,hd)
        let att = q.matmul(&k.t()?)?; // (b,nh,tq,t)
        drop((q, k));
        att.inplace_op1(&MaskedSoftmax {
            lens,
            rows_per_seq: nh * tq,
            t,
        })?;
        let ctx = att.matmul(&v)?; // (b,nh,tq,hd)
        drop((att, v, qkv));
        let ctx = ctx.transpose(1, 2)?.contiguous()?.reshape((b * tq, h))?;
        let resid = if cls_only {
            x.reshape((b, t, h))?
                .narrow(1, 0, 1)?
                .contiguous()?
                .reshape((b, h))?
        } else {
            x.clone()
        };
        let a = ctx.matmul(&l.wo.t()?)?;
        drop(ctx);
        a.inplace_op2(
            &resid,
            &BiasResLn {
                bias: &l.bo,
                g: &l.ln1_g,
                beta: &l.ln1_b,
                eps: self.eps,
            },
        )?;
        drop(resid);
        let f = a.matmul(&l.w1.t()?)?;
        f.inplace_op1(&BiasAct {
            bias: &l.b1,
            gelu: true,
        })?;
        let o = f.matmul(&l.w2.t()?)?;
        drop(f);
        o.inplace_op2(
            &a,
            &BiasResLn {
                bias: &l.b2,
                g: &l.ln2_g,
                beta: &l.ln2_b,
                eps: self.eps,
            },
        )?;
        Ok(o)
    }
}

enum EncoderImpl {
    Cpu(CpuEncoder),
    Metal(crate::tensor_encoder::TensorEncoder),
}

/// Shared interface with an unchanged CPU path and an entirely device-resident Metal path.
pub(crate) struct Encoder {
    pub cfg: XlmrConfig,
    pub max_len: usize,
    device: Device,
    inner: EncoderImpl,
}

impl Encoder {
    pub fn load_on(
        cfg: XlmrConfig,
        src: &Source,
        prefix: &str,
        max_len: usize,
        device: &Device,
    ) -> Result<Self> {
        let max_len = max_len.min(cfg.max_tokens());
        if max_len < 2 {
            bail!("max_len too small");
        }
        let inner = if device.is_cpu() {
            EncoderImpl::Cpu(CpuEncoder::load(cfg.clone(), src, prefix, max_len)?)
        } else if device.is_metal() {
            EncoderImpl::Metal(crate::tensor_encoder::TensorEncoder::load(
                cfg.clone(),
                src,
                prefix,
                max_len,
                device,
            )?)
        } else {
            bail!("only CPU and Metal inference devices are supported");
        };
        Ok(Self {
            cfg,
            max_len,
            device: device.clone(),
            inner,
        })
    }
    pub fn device(&self) -> &Device {
        &self.device
    }
    pub fn hidden_size(&self) -> usize {
        self.cfg.hidden_size
    }
    pub fn forward_cls(&self, seqs: &[&[u32]]) -> Result<Tensor> {
        match &self.inner {
            EncoderImpl::Cpu(encoder) => encoder.forward_cls(seqs),
            EncoderImpl::Metal(encoder) => encoder.forward_cls(seqs),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Fused in-place CPU ops.

fn f32_mut<'a>(s: &'a mut CpuStorage, l: &Layout) -> candle_core::Result<&'a mut [f32]> {
    let (start, end) = l
        .contiguous_offsets()
        .ok_or_else(|| candle_core::Error::Msg("expected contiguous tensor".into()))?;
    match s {
        CpuStorage::F32(v) => Ok(&mut v[start..end]),
        _ => Err(candle_core::Error::Msg("expected f32 storage".into())),
    }
}

fn f32_ref<'a>(s: &'a CpuStorage, l: &Layout) -> candle_core::Result<&'a [f32]> {
    let (start, end) = l
        .contiguous_offsets()
        .ok_or_else(|| candle_core::Error::Msg("expected contiguous tensor".into()))?;
    match s {
        CpuStorage::F32(v) => Ok(&v[start..end]),
        _ => Err(candle_core::Error::Msg("expected f32 storage".into())),
    }
}

/// Rows per rayon task so that each task touches ~64 KB.
fn rows_per_task(cols: usize) -> usize {
    (16384 / cols.max(1)).max(1)
}

/// x[r, :] += bias, optionally followed by exact GELU.
struct BiasAct<'a> {
    bias: &'a [f32],
    gelu: bool,
}

impl InplaceOp1 for BiasAct<'_> {
    fn name(&self) -> &'static str {
        "memory-bias-act"
    }
    fn cpu_fwd(&self, s: &mut CpuStorage, l: &Layout) -> candle_core::Result<()> {
        let x = f32_mut(s, l)?;
        let c = self.bias.len();
        if x.len() % c != 0 {
            return Err(candle_core::Error::Msg("bias-act: size mismatch".into()));
        }
        let bias = self.bias;
        let gelu = self.gelu;
        x.par_chunks_mut(c * rows_per_task(c)).for_each(|chunk| {
            for row in chunk.chunks_exact_mut(c) {
                if gelu {
                    kernels::bias_gelu_row(row, bias);
                } else {
                    kernels::bias_row(row, bias);
                }
            }
        });
        Ok(())
    }
}

/// x = LayerNorm(x + bias + residual) * g + beta, row-wise.
struct BiasResLn<'a> {
    bias: &'a [f32],
    g: &'a [f32],
    beta: &'a [f32],
    eps: f32,
}

impl InplaceOp2 for BiasResLn<'_> {
    fn name(&self) -> &'static str {
        "memory-bias-residual-layernorm"
    }
    fn cpu_fwd(
        &self,
        s1: &mut CpuStorage,
        l1: &Layout,
        s2: &CpuStorage,
        l2: &Layout,
    ) -> candle_core::Result<()> {
        let x = f32_mut(s1, l1)?;
        let r = f32_ref(s2, l2)?;
        let c = self.bias.len();
        if x.len() != r.len() || x.len() % c != 0 {
            return Err(candle_core::Error::Msg("bias-res-ln: size mismatch".into()));
        }
        let n = c * rows_per_task(c);
        x.par_chunks_mut(n)
            .zip(r.par_chunks(n))
            .for_each(|(xc, rc)| {
                for (xr, rr) in xc.chunks_exact_mut(c).zip(rc.chunks_exact(c)) {
                    kernels::add2_row(xr, self.bias, rr);
                    kernels::layer_norm_row(xr, self.g, self.beta, self.eps);
                }
            });
        Ok(())
    }
}

/// Row-wise softmax over the first `lens[seq]` columns; padded key columns get probability 0.
/// Layout: (b, rows_per_seq, t) contiguous.
struct MaskedSoftmax<'a> {
    lens: &'a [usize],
    rows_per_seq: usize,
    t: usize,
}

impl InplaceOp1 for MaskedSoftmax<'_> {
    fn name(&self) -> &'static str {
        "memory-masked-softmax"
    }
    fn cpu_fwd(&self, s: &mut CpuStorage, l: &Layout) -> candle_core::Result<()> {
        let x = f32_mut(s, l)?;
        let t = self.t;
        if x.len() != self.lens.len() * self.rows_per_seq * t {
            return Err(candle_core::Error::Msg(
                "masked-softmax: size mismatch".into(),
            ));
        }
        let rpt = rows_per_task(t);
        x.par_chunks_mut(t * rpt)
            .enumerate()
            .for_each(|(ci, chunk)| {
                for (ri, row) in chunk.chunks_exact_mut(t).enumerate() {
                    let r = ci * rpt + ri;
                    let valid = self.lens[r / self.rows_per_seq].min(t);
                    let (live, dead) = row.split_at_mut(valid);
                    kernels::softmax_row(live);
                    dead.fill(0.0);
                }
            });
        Ok(())
    }
}
