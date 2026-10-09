//! Device-resident XLM-R encoder. All weights and intermediate tensors stay on the selected GPU.
//! Only small input-id/position/mask arrays originate on CPU; the caller copies final outputs back.
use crate::{weights::Source, xlmr::XlmrConfig};
use anyhow::{bail, Result};
use candle_core::{Device, Tensor};

struct Layer {
    wqkv: Tensor,
    bqkv: Tensor,
    wo: Tensor,
    bo: Tensor,
    ln1_g: Tensor,
    ln1_b: Tensor,
    w1: Tensor,
    b1: Tensor,
    w2: Tensor,
    b2: Tensor,
    ln2_g: Tensor,
    ln2_b: Tensor,
}

pub(crate) struct TensorEncoder {
    device: Device,
    cfg: XlmrConfig,
    max_len: usize,
    word: Tensor,
    pos: Tensor,
    types: Tensor,
    ln_g: Tensor,
    ln_b: Tensor,
    layers: Vec<Layer>,
}

fn get(src: &Source, name: &str, shape: &[usize], device: &Device) -> Result<Tensor> {
    let tensor = src.get(name)?;
    if tensor.dims() != shape {
        bail!(
            "unexpected shape for {name}: {:?}, expected {shape:?}",
            tensor.dims()
        );
    }
    Ok(tensor.to_device(device)?)
}

fn ln(src: &Source, name: &str, hidden: usize, device: &Device) -> Result<(Tensor, Tensor)> {
    let (weight, bias) = if src.contains(&format!("{name}.weight")) {
        ("weight", "bias")
    } else {
        ("gamma", "beta")
    };
    Ok((
        get(src, &format!("{name}.{weight}"), &[hidden], device)?,
        get(src, &format!("{name}.{bias}"), &[hidden], device)?,
    ))
}

impl TensorEncoder {
    pub(crate) fn load(
        cfg: XlmrConfig,
        src: &Source,
        prefix: &str,
        max_len: usize,
        device: &Device,
    ) -> Result<Self> {
        let h = cfg.hidden_size;
        let e = format!("{prefix}embeddings");
        let word = get(
            src,
            &format!("{e}.word_embeddings.weight"),
            &[cfg.vocab_size, h],
            device,
        )?;
        let pos = get(
            src,
            &format!("{e}.position_embeddings.weight"),
            &[cfg.max_position_embeddings, h],
            device,
        )?
        .narrow(0, 0, cfg.pad_token_id as usize + 1 + max_len)?
        .contiguous()?;
        let types = get(
            src,
            &format!("{e}.token_type_embeddings.weight"),
            &[cfg.type_vocab_size, h],
            device,
        )?
        .narrow(0, 0, 1)?
        .contiguous()?;
        let (ln_g, ln_b) = ln(src, &format!("{e}.LayerNorm"), h, device)?;
        let scale = 1.0 / ((h / cfg.num_attention_heads) as f64).sqrt();
        let mut layers = Vec::with_capacity(cfg.num_hidden_layers);
        for i in 0..cfg.num_hidden_layers {
            let p = format!("{prefix}encoder.layer.{i}");
            let a = format!("{p}.attention.self");
            let wq = (get(src, &format!("{a}.query.weight"), &[h, h], device)? * scale)?;
            let wk = get(src, &format!("{a}.key.weight"), &[h, h], device)?;
            let wv = get(src, &format!("{a}.value.weight"), &[h, h], device)?;
            let bq = (get(src, &format!("{a}.query.bias"), &[h], device)? * scale)?;
            let bk = get(src, &format!("{a}.key.bias"), &[h], device)?;
            let bv = get(src, &format!("{a}.value.bias"), &[h], device)?;
            let o = format!("{p}.attention.output");
            let (ln1_g, ln1_b) = ln(src, &format!("{o}.LayerNorm"), h, device)?;
            let (ln2_g, ln2_b) = ln(src, &format!("{p}.output.LayerNorm"), h, device)?;
            let inter = cfg.intermediate_size;
            layers.push(Layer {
                wqkv: Tensor::cat(&[&wq, &wk, &wv], 0)?.contiguous()?,
                bqkv: Tensor::cat(&[&bq, &bk, &bv], 0)?.contiguous()?,
                wo: get(src, &format!("{o}.dense.weight"), &[h, h], device)?,
                bo: get(src, &format!("{o}.dense.bias"), &[h], device)?,
                ln1_g,
                ln1_b,
                w1: get(
                    src,
                    &format!("{p}.intermediate.dense.weight"),
                    &[inter, h],
                    device,
                )?,
                b1: get(
                    src,
                    &format!("{p}.intermediate.dense.bias"),
                    &[inter],
                    device,
                )?,
                w2: get(
                    src,
                    &format!("{p}.output.dense.weight"),
                    &[h, inter],
                    device,
                )?,
                b2: get(src, &format!("{p}.output.dense.bias"), &[h], device)?,
                ln2_g,
                ln2_b,
            });
        }
        Ok(Self {
            device: device.clone(),
            cfg,
            max_len,
            word,
            pos,
            types,
            ln_g,
            ln_b,
            layers,
        })
    }

    pub(crate) fn forward_cls(&self, seqs: &[&[u32]]) -> Result<Tensor> {
        if seqs.is_empty() || seqs.iter().any(|s| s.is_empty()) {
            bail!("empty batch/token sequence");
        }
        let (b, t, h) = (
            seqs.len(),
            seqs.iter().map(|s| s.len()).max().unwrap(),
            self.cfg.hidden_size,
        );
        if t > self.max_len {
            bail!("sequence longer than max length");
        }
        let mut ids = Vec::with_capacity(b * t);
        let mut positions = Vec::with_capacity(b * t);
        let mut mask = Vec::with_capacity(b * t);
        for seq in seqs {
            let mut count = 0u32;
            for j in 0..t {
                let id = seq.get(j).copied().unwrap_or(self.cfg.pad_token_id);
                if id as usize >= self.cfg.vocab_size {
                    bail!("token id {id} outside vocabulary");
                }
                ids.push(id);
                if id != self.cfg.pad_token_id {
                    count += 1;
                }
                positions.push(if id == self.cfg.pad_token_id {
                    self.cfg.pad_token_id
                } else {
                    self.cfg.pad_token_id + count
                });
                mask.push(if j < seq.len() {
                    0f32
                } else {
                    f32::NEG_INFINITY
                });
            }
        }
        let ids = Tensor::from_vec(ids, b * t, &self.device)?;
        let positions = Tensor::from_vec(positions, b * t, &self.device)?;
        let mask = Tensor::from_vec(mask, (b, 1, 1, t), &self.device)?;
        let embeddings = self
            .word
            .index_select(&ids, 0)?
            .add(&self.pos.index_select(&positions, 0)?)?
            .broadcast_add(&self.types)?;
        let mut x = candle_nn::ops::layer_norm(
            &embeddings,
            &self.ln_g,
            &self.ln_b,
            self.cfg.layer_norm_eps as f32,
        )?;
        let (nh, hd) = (
            self.cfg.num_attention_heads,
            h / self.cfg.num_attention_heads,
        );
        for (index, layer) in self.layers.iter().enumerate() {
            let cls_only = index + 1 == self.layers.len();
            let tq = if cls_only { 1 } else { t };
            let qkv = x
                .matmul(&layer.wqkv.t()?)?
                .broadcast_add(&layer.bqkv)?
                .reshape((b, t, 3, nh, hd))?;
            let q = qkv
                .narrow(1, 0, tq)?
                .narrow(2, 0, 1)?
                .squeeze(2)?
                .transpose(1, 2)?
                .contiguous()?;
            let k = qkv
                .narrow(2, 1, 1)?
                .squeeze(2)?
                .transpose(1, 2)?
                .contiguous()?;
            let v = qkv
                .narrow(2, 2, 1)?
                .squeeze(2)?
                .transpose(1, 2)?
                .contiguous()?;
            let scores = q.matmul(&k.t()?)?.broadcast_add(&mask)?.contiguous()?;
            let attention = candle_nn::ops::softmax_last_dim(&scores)?;
            let context = attention
                .matmul(&v)?
                .transpose(1, 2)?
                .contiguous()?
                .reshape((b * tq, h))?;
            let residual = if cls_only {
                x.reshape((b, t, h))?
                    .narrow(1, 0, 1)?
                    .contiguous()?
                    .reshape((b, h))?
            } else {
                x.clone()
            };
            let attention_output = context
                .matmul(&layer.wo.t()?)?
                .broadcast_add(&layer.bo)?
                .add(&residual)?;
            let a = candle_nn::ops::layer_norm(
                &attention_output,
                &layer.ln1_g,
                &layer.ln1_b,
                self.cfg.layer_norm_eps as f32,
            )?;
            let f = a
                .matmul(&layer.w1.t()?)?
                .broadcast_add(&layer.b1)?
                .gelu_erf()?;
            let o = f
                .matmul(&layer.w2.t()?)?
                .broadcast_add(&layer.b2)?
                .add(&a)?;
            x = candle_nn::ops::layer_norm(
                &o,
                &layer.ln2_g,
                &layer.ln2_b,
                self.cfg.layer_norm_eps as f32,
            )?;
        }
        Ok(x)
    }
}
