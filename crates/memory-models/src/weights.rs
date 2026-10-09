//! Weight loading: memory-mapped safetensors (preferred) or PyTorch `pytorch_model.bin`, plus a
//! bounded-memory converter from `.bin` to `.safetensors`.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use candle_core::pickle::PthTensors;
use candle_core::safetensors::MmapedSafetensors;
use candle_core::{DType, Device, Tensor};

pub(crate) const SAFETENSORS_NAME: &str = "model.safetensors";
pub(crate) const PTH_NAME: &str = "pytorch_model.bin";

/// A source of named tensors.
pub(crate) enum Source {
    St(MmapedSafetensors, PathBuf),
    Pth(PthTensors),
}

impl Source {
    /// Open the weights in `dir`: `model.safetensors` if present, otherwise `pytorch_model.bin`.
    pub(crate) fn open(dir: &Path) -> Result<Self> {
        let st = dir.join(SAFETENSORS_NAME);
        if st.is_file() {
            return Self::open_safetensors(&st);
        }
        let pth = dir.join(PTH_NAME);
        if pth.is_file() {
            let p = PthTensors::new(&pth, None)
                .with_context(|| format!("reading pickle tensor index of {}", pth.display()))?;
            return Ok(Source::Pth(p));
        }
        bail!(
            "no weights in {} (expected {} or {})",
            dir.display(),
            SAFETENSORS_NAME,
            PTH_NAME
        )
    }

    pub(crate) fn open_safetensors(path: &Path) -> Result<Self> {
        // SAFETY: the weight files are treated as read-only for the lifetime of the process.
        let st = unsafe { MmapedSafetensors::new(path) }
            .with_context(|| format!("mmap {}", path.display()))?;
        Ok(Source::St(st, path.to_path_buf()))
    }

    pub(crate) fn names(&self) -> Vec<String> {
        match self {
            Source::St(st, _) => st.tensors().into_iter().map(|(n, _)| n).collect(),
            Source::Pth(p) => p.tensor_infos().keys().cloned().collect(),
        }
    }

    pub(crate) fn contains(&self, name: &str) -> bool {
        match self {
            Source::St(st, _) => st.get(name).is_ok(),
            Source::Pth(p) => p.tensor_infos().contains_key(name),
        }
    }

    /// Load a tensor as contiguous F32 on the CPU.
    pub(crate) fn get(&self, name: &str) -> Result<Tensor> {
        let t = match self {
            Source::St(st, _) => st
                .load(name, &Device::Cpu)
                .with_context(|| format!("tensor {name}"))?,
            Source::Pth(p) => p
                .get(name)
                .with_context(|| format!("tensor {name}"))?
                .with_context(|| format!("tensor {name} missing"))?,
        };
        let t = if t.dtype() == DType::F32 {
            t
        } else {
            t.to_dtype(DType::F32)?
        };
        Ok(t.contiguous()?)
    }

    pub(crate) fn get_vec(&self, name: &str) -> Result<Vec<f32>> {
        Ok(self.get(name)?.flatten_all()?.to_vec1::<f32>()?)
    }
}

/// Convert a PyTorch `pytorch_model.bin` into a safetensors file, one tensor at a time (peak
/// memory ~ the largest tensor). Non-float tensors (e.g. `position_ids` buffers) are skipped.
/// Writes to `<out>.tmp` and renames, so a crash never leaves a truncated `model.safetensors`.
pub fn convert_pth_to_safetensors(pth: &Path, out: &Path) -> Result<()> {
    let p = PthTensors::new(pth, None)
        .with_context(|| format!("reading pickle tensor index of {}", pth.display()))?;
    let mut entries: BTreeMap<String, (DType, Vec<usize>)> = BTreeMap::new();
    for (name, info) in p.tensor_infos() {
        let dt = info.dtype;
        if !matches!(dt, DType::F32 | DType::F16 | DType::BF16 | DType::F64) {
            continue;
        }
        entries.insert(name.clone(), (dt, info.layout.dims().to_vec()));
    }
    if entries.is_empty() {
        bail!("no float tensors found in {}", pth.display());
    }
    // Everything is written as F32 (models are used in f32 anyway).
    let mut header = serde_json::Map::new();
    let mut offset = 0usize;
    for (name, (_, shape)) in &entries {
        let n: usize = shape.iter().product();
        let bytes = n * 4;
        header.insert(
            name.clone(),
            serde_json::json!({"dtype": "F32", "shape": shape, "data_offsets": [offset, offset + bytes]}),
        );
        offset += bytes;
    }
    header.insert("__metadata__".into(), serde_json::json!({"format": "pt"}));
    let mut hbytes = serde_json::to_vec(&serde_json::Value::Object(header))?;
    while (8 + hbytes.len()) % 8 != 0 {
        hbytes.push(b' ');
    }

    let tmp = out.with_extension("safetensors.tmp");
    let f = std::fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
    let mut w = std::io::BufWriter::with_capacity(1 << 22, f);
    w.write_all(&(hbytes.len() as u64).to_le_bytes())?;
    w.write_all(&hbytes)?;
    for (name, (_, shape)) in &entries {
        let t = p
            .get(name)?
            .with_context(|| format!("tensor {name} missing"))?;
        let t = if t.dtype() == DType::F32 {
            t
        } else {
            t.to_dtype(DType::F32)?
        };
        let t = t.contiguous()?;
        if t.dims() != shape.as_slice() {
            bail!("shape mismatch for {name}: {:?} vs {:?}", t.dims(), shape);
        }
        let (storage, layout) = t.storage_and_layout();
        let data = match &*storage {
            candle_core::Storage::Cpu(candle_core::CpuStorage::F32(v)) => v,
            _ => bail!("unexpected storage for {name}"),
        };
        let (start, end) = layout
            .contiguous_offsets()
            .context("non-contiguous tensor after contiguous()")?;
        let slice = &data[start..end];
        // SAFETY: f32 slice reinterpreted as bytes (little-endian host; safetensors is LE).
        let bytes = unsafe {
            std::slice::from_raw_parts(slice.as_ptr() as *const u8, std::mem::size_of_val(slice))
        };
        w.write_all(bytes)?;
    }
    w.flush()?;
    let f = w.into_inner().map_err(|e| anyhow::anyhow!("flush: {e}"))?;
    f.sync_all()?;
    drop(f);
    // Validate before publishing.
    {
        let check = unsafe { MmapedSafetensors::new(&tmp) }.context("re-open converted file")?;
        let n = check.tensors().len();
        if n != entries.len() {
            bail!("converted file has {n} tensors, expected {}", entries.len());
        }
    }
    std::fs::rename(&tmp, out).with_context(|| format!("rename to {}", out.display()))?;
    Ok(())
}

/// Ensure `dir/model.safetensors` exists, converting from `pytorch_model.bin` if needed.
/// Returns Ok(true) if a conversion was performed.
pub fn ensure_safetensors(dir: &Path) -> Result<bool> {
    let st = dir.join(SAFETENSORS_NAME);
    if st.is_file() {
        return Ok(false);
    }
    let pth = dir.join(PTH_NAME);
    if !pth.is_file() {
        bail!(
            "neither {} nor {} in {}",
            SAFETENSORS_NAME,
            PTH_NAME,
            dir.display()
        );
    }
    convert_pth_to_safetensors(&pth, &st)?;
    Ok(true)
}
