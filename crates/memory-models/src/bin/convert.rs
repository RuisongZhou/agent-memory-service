//! `memory-models-convert <model_dir | pytorch_model.bin> [out.safetensors] [--force]`
//!
//! Converts a PyTorch `pytorch_model.bin` checkpoint into `model.safetensors` (F32, one tensor at
//! a time, so peak memory is about the size of the largest tensor). With a directory argument the
//! output is `<dir>/model.safetensors`; an existing output is kept unless `--force` is given.
//! The library does the same conversion automatically on first load.

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{bail, Result};

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let force = if let Some(p) = args.iter().position(|a| a == "--force") {
        args.remove(p);
        true
    } else {
        false
    };
    if args.is_empty() || args.len() > 2 || args.iter().any(|a| a == "-h" || a == "--help") {
        eprintln!("usage: memory-models-convert <model_dir | pytorch_model.bin> [out.safetensors] [--force]");
        std::process::exit(2);
    }
    let input = PathBuf::from(&args[0]);
    let (pth, default_out) = if input.is_dir() {
        (
            input.join("pytorch_model.bin"),
            input.join("model.safetensors"),
        )
    } else {
        let dir = input.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        (input.clone(), dir.join("model.safetensors"))
    };
    let out = args.get(1).map(PathBuf::from).unwrap_or(default_out);
    if !pth.is_file() {
        bail!("{} not found", pth.display());
    }
    if out.exists() && !force {
        eprintln!(
            "{} already exists (use --force to overwrite)",
            out.display()
        );
        return Ok(());
    }
    let t0 = Instant::now();
    memory_models::convert_pth_to_safetensors(&pth, &out)?;
    let size = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
    eprintln!(
        "wrote {} ({:.1} MB) in {:.1}s, peak RSS {:.0} MiB",
        out.display(),
        size as f64 / 1e6,
        t0.elapsed().as_secs_f64(),
        memory_models::peak_rss_mib().unwrap_or(0.0)
    );
    Ok(())
}
