//! `memory-models-bench` — throughput / latency benchmark and reference dumps.
//!
//! ```text
//! memory-models-bench embed  <model_dir> [--batch 32] [--rounds 5] [--warmup 1]
//! memory-models-bench rerank <model_dir> [--docs 30]  [--rounds 5] [--warmup 1]
//! memory-models-bench dump-embed  <model_dir> <texts.json> <out.json>   # ["text", ...]
//! memory-models-bench dump-rerank <model_dir> <pairs.json> <out.json>   # [["query","doc"], ...]
//! ```
//! Benchmark texts are LoCoMo-style chat utterances of roughly 30-60 tokens. Thread count follows
//! `RAYON_NUM_THREADS` (default: all cores).

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use memory_models::{
    peak_rss_mib, rss_mib, runtime_backend, runtime_device, select_device, DeviceChoice, Embedder,
    Reranker,
};

const SPEAKERS: &[&str] = &[
    "Caroline", "Melanie", "Jon", "Gina", "John", "Maria", "Joanna", "Nate",
];

const SENTENCES: &[&str] = &[
    "I went to the LGBTQ support group yesterday and it was so powerful to hear everyone's stories about coming out.",
    "We took the kids camping at the beach last weekend, roasted marshmallows and watched the stars until midnight.",
    "I've been painting a lot lately, mostly sunsets and lakes, it really helps me relax after a long week at work.",
    "I'm researching adoption agencies because I want to give a child a loving home, it's a big step but I'm ready.",
    "My dance studio finally opened last month, and the first class was packed with kids from the neighborhood.",
    "I lost my job at the bank in January, so I decided to start my own business selling handmade clothing online.",
    "We visited my grandma in Sweden over the summer; she taught me how to bake cinnamon buns from her old recipe.",
    "The concert was amazing, they played all their old songs and my daughter knew every single word by heart.",
    "I signed up for a pottery class on Thursdays, my first bowl came out lopsided but I'm still proud of it.",
    "I've been reading a book about mindfulness and it reminded me to slow down and enjoy the little moments.",
    "Our road trip through the national parks was exhausting but the hikes and the views were totally worth it.",
    "I just got promoted to team lead, which means more meetings, but I'm excited to mentor the new engineers.",
    "My counseling sessions have helped me a lot this year, I feel much more confident talking about my feelings.",
    "We adopted a puppy named Oliver from the shelter, he chews everything but the kids absolutely adore him.",
    "I ran my first half marathon on Sunday in two hours and five minutes, my legs are still sore today.",
    "I'm volunteering at the youth center on weekends, helping teenagers with homework and college applications.",
];

const QUERIES: &[&str] = &[
    "When did Caroline go to the LGBTQ support group?",
    "What does Melanie like to paint?",
    "Why did Jon start his own business?",
];

/// Deterministic LoCoMo-style utterances: "<Speaker>: <sentence> <tail>" (≈ 30-60 tokens).
fn sample_texts(n: usize) -> Vec<String> {
    let tails = [
        "",
        " How about you?",
        " Anyway, how have you been lately?",
        " I'll send you some photos later tonight.",
        " It's been such a busy month for all of us.",
    ];
    (0..n)
        .map(|i| {
            let s = SPEAKERS[i % SPEAKERS.len()];
            let a = SENTENCES[(i * 7 + i / SENTENCES.len()) % SENTENCES.len()];
            let t = tails[(i * 3) % tails.len()];
            format!("{s}: {a}{t}")
        })
        .collect()
}

struct Args {
    batch: usize,
    docs: usize,
    rounds: usize,
    warmup: usize,
    pos: Vec<String>,
    device: DeviceChoice,
}

fn parse_args(raw: &[String]) -> Result<Args> {
    let mut a = Args {
        batch: 32,
        docs: 30,
        rounds: 5,
        warmup: 1,
        pos: Vec::new(),
        device: DeviceChoice::Cpu,
    };
    let mut it = raw.iter();
    while let Some(x) = it.next() {
        let mut num = |name: &str| -> Result<usize> {
            it.next()
                .with_context(|| format!("{name} needs a value"))?
                .parse()
                .with_context(|| format!("bad {name}"))
        };
        match x.as_str() {
            "--batch" => a.batch = num("--batch")?,
            "--docs" => a.docs = num("--docs")?,
            "--rounds" => a.rounds = num("--rounds")?,
            "--warmup" => a.warmup = num("--warmup")?,
            "--device" => {
                a.device = it
                    .next()
                    .context("--device needs cpu|metal|auto")?
                    .parse()?
            }
            _ if x.starts_with("--") => bail!("unknown option {x:?}"),
            _ => a.pos.push(x.clone()),
        }
    }
    Ok(a)
}

fn stats(v: &mut [f64]) -> (f64, f64, f64) {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = v.iter().sum::<f64>() / v.len() as f64;
    (mean, v[v.len() / 2], v[0])
}

fn threads() -> String {
    std::env::var("RAYON_NUM_THREADS")
        .unwrap_or_else(|_| format!("{} (default)", rayon::current_num_threads()))
}

fn runtime(device: &candle_core::Device) {
    println!(
        "runtime {}",
        serde_json::json!({"backend":runtime_backend(device),"device":runtime_device(device),"dtype":"float32"})
    );
}

fn token_summary(lens: &[usize]) -> String {
    let min = lens.iter().min().copied().unwrap_or(0);
    let max = lens.iter().max().copied().unwrap_or(0);
    let mean = lens.iter().sum::<usize>() as f64 / lens.len().max(1) as f64;
    format!("tokens/seq min {min} mean {mean:.1} max {max}")
}

fn bench_embed(dir: &PathBuf, a: &Args) -> Result<()> {
    if a.rounds == 0 || a.batch == 0 {
        bail!("embed requires --rounds > 0 and --batch > 0");
    }
    let t0 = Instant::now();
    let e = Embedder::load_on(dir, &select_device(a.device)?)?;
    runtime(e.device());
    let load_s = t0.elapsed().as_secs_f64();
    println!(
        "model {} dim {} | load {:.2}s, RSS after load {:.0} MiB | threads {} | backend {}",
        e.model_id(),
        e.dim(),
        load_s,
        rss_mib().unwrap_or(0.0),
        threads(),
        e.backend()
    );
    let texts = sample_texts(a.batch);
    let lens: Vec<usize> = e.tokenize(&texts)?.iter().map(|s| s.len()).collect();
    println!("batch {} texts, {}", a.batch, token_summary(&lens));
    for _ in 0..a.warmup {
        e.embed(&texts)?;
    }
    let mut times = Vec::new();
    for r in 0..a.rounds {
        // vary the texts a little between rounds (same length profile)
        let texts: Vec<String> = texts.iter().map(|t| format!("{t} ({r})")).collect();
        let t = Instant::now();
        let v = e.embed(&texts)?;
        times.push(t.elapsed().as_secs_f64());
        assert_eq!(v.len(), texts.len());
    }
    let (mean, med, best) = stats(&mut times);
    println!(
        "embed: {:.2} s/batch mean (median {:.2}, best {:.2}) → {:.1} texts/s (median {:.1}) | peak RSS {:.0} MiB",
        mean,
        med,
        best,
        a.batch as f64 / mean,
        a.batch as f64 / med,
        peak_rss_mib().unwrap_or(0.0)
    );
    // single short query latency (search path)
    let mut q = Vec::new();
    for (i, s) in QUERIES.iter().enumerate() {
        let t = Instant::now();
        e.embed(&[format!("{s} #{i}")])?;
        q.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    let (qm, _, _) = stats(&mut q);
    println!("single query embed: {qm:.0} ms mean");
    Ok(())
}

fn bench_rerank(dir: &PathBuf, a: &Args) -> Result<()> {
    if a.rounds == 0 || a.docs == 0 {
        bail!("rerank requires --rounds > 0 and --docs > 0");
    }
    let t0 = Instant::now();
    let r = Reranker::load_on(dir, &select_device(a.device)?)?;
    runtime(r.device());
    let load_s = t0.elapsed().as_secs_f64();
    println!(
        "model {} | load {:.2}s, RSS after load {:.0} MiB | threads {} | backend {}",
        r.model_id(),
        load_s,
        rss_mib().unwrap_or(0.0),
        threads(),
        r.backend()
    );
    let docs = sample_texts(a.docs);
    let lens: Vec<usize> = r
        .tokenize_pairs(QUERIES[0], &docs)?
        .iter()
        .map(|s| s.len())
        .collect();
    println!("1 query x {} docs, pair {}", a.docs, token_summary(&lens));
    for _ in 0..a.warmup {
        r.score(QUERIES[0], &docs)?;
    }
    let mut times = Vec::new();
    for i in 0..a.rounds {
        let q = QUERIES[i % QUERIES.len()];
        let t = Instant::now();
        let s = r.score(q, &docs)?;
        times.push(t.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(s.len(), docs.len());
    }
    let (mean, med, best) = stats(&mut times);
    println!(
        "rerank {} pairs: {:.0} ms mean (median {:.0}, best {:.0}) → {:.1} pairs/s | peak RSS {:.0} MiB",
        a.docs,
        mean,
        med,
        best,
        a.docs as f64 * 1000.0 / mean,
        peak_rss_mib().unwrap_or(0.0)
    );
    let s = r.score(QUERIES[0], &docs[..docs.len().min(4)])?;
    println!("sample scores for {:?}: {:?}", QUERIES[0], s);
    Ok(())
}

/// `gemm <m>`: GFLOP/s of the encoder's matmul shapes (x[m,K] · W[N,K]^T) with candle's CPU matmul.
fn bench_gemm(a: &Args) -> Result<()> {
    use candle_core::{Device, Tensor};
    let m: usize = a
        .pos
        .first()
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(1536);
    if m == 0 || a.rounds == 0 {
        bail!("gemm requires m > 0 and --rounds > 0");
    }
    for (n, k) in [
        (3072usize, 1024usize),
        (1024, 1024),
        (4096, 1024),
        (1024, 4096),
    ] {
        let x = Tensor::randn(0f32, 1.0, (m, k), &Device::Cpu)?;
        let w = Tensor::randn(0f32, 1.0, (n, k), &Device::Cpu)?;
        let wt = w.t()?;
        let _ = x.matmul(&wt)?;
        let t = Instant::now();
        let reps = a.rounds.max(1);
        for _ in 0..reps {
            let _ = x.matmul(&wt)?;
        }
        let s = t.elapsed().as_secs_f64() / reps as f64;
        println!(
            "m {m} n {n} k {k}: {:.1} ms, {:.1} GFLOP/s",
            s * 1000.0,
            2.0 * (m * n * k) as f64 / s / 1e9
        );
    }
    Ok(())
}

fn dump_embed(dir: &PathBuf, input: &str, out: &str, device: DeviceChoice) -> Result<()> {
    let texts: Vec<String> = serde_json::from_str(&std::fs::read_to_string(input)?)?;
    if texts.is_empty() {
        bail!("reference texts must be nonempty");
    }
    let e = Embedder::load_on(dir, &select_device(device)?)?;
    runtime(e.device());
    let one_by_one: Vec<Vec<f32>> = texts
        .iter()
        .map(|t| e.embed(std::slice::from_ref(t)).map(|mut v| v.remove(0)))
        .collect::<Result<_>>()?;
    let batched = e.embed(&texts)?;
    let ids = e.tokenize(&texts)?;
    let j = serde_json::json!({"model": e.model_id(), "single": one_by_one, "batched": batched, "ids": ids,
        "backend":e.backend(),"device":runtime_device(e.device()),"dtype":"float32"});
    std::fs::write(out, serde_json::to_string(&j)?)?;
    eprintln!(
        "wrote {out} (peak RSS {:.0} MiB)",
        peak_rss_mib().unwrap_or(0.0)
    );
    Ok(())
}

fn dump_rerank(dir: &PathBuf, input: &str, out: &str, device: DeviceChoice) -> Result<()> {
    let pairs: Vec<(String, String)> = serde_json::from_str(&std::fs::read_to_string(input)?)?;
    if pairs.is_empty() {
        bail!("reference pairs must be nonempty");
    }
    let r = Reranker::load_on(dir, &select_device(device)?)?;
    runtime(r.device());
    let mut single = Vec::new();
    let mut ids = Vec::new();
    for (q, d) in &pairs {
        single.push(r.score(q, std::slice::from_ref(d))?[0]);
        ids.push(r.tokenize_pairs(q, std::slice::from_ref(d))?.remove(0));
    }
    // batched: group by query (as the daemon calls it)
    let mut batched = vec![0f32; pairs.len()];
    let mut queries: Vec<&String> = pairs.iter().map(|p| &p.0).collect();
    let mut seen = std::collections::HashSet::new();
    queries.retain(|q| seen.insert((*q).clone()));
    for q in queries {
        let idx: Vec<usize> = (0..pairs.len()).filter(|&i| &pairs[i].0 == q).collect();
        let docs: Vec<String> = idx.iter().map(|&i| pairs[i].1.clone()).collect();
        for (k, s) in r.score(q, &docs)?.into_iter().enumerate() {
            batched[idx[k]] = s;
        }
    }
    let j = serde_json::json!({"model": r.model_id(), "single": single, "batched": batched, "ids": ids,
        "backend":r.backend(),"device":runtime_device(r.device()),"dtype":"float32"});
    std::fs::write(out, serde_json::to_string(&j)?)?;
    eprintln!(
        "wrote {out} (peak RSS {:.0} MiB)",
        peak_rss_mib().unwrap_or(0.0)
    );
    Ok(())
}

fn main() -> Result<()> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.is_empty() {
        eprintln!(
            "usage: memory-models-bench embed|rerank <model_dir> [--batch N] [--docs N] [--rounds N] [--warmup N]\n       \
             memory-models-bench dump-embed|dump-rerank <model_dir> <in.json> <out.json>"
        );
        std::process::exit(2);
    }
    let cmd = raw[0].clone();
    let a = parse_args(&raw[1..])?;
    if cmd == "gemm" {
        return bench_gemm(&a);
    }
    let dir = PathBuf::from(a.pos.first().context("model_dir missing")?);
    match cmd.as_str() {
        "gemm" => bench_gemm(&a),
        "embed" => bench_embed(&dir, &a),
        "rerank" => bench_rerank(&dir, &a),
        "dump-embed" | "dump-rerank" => {
            if a.pos.len() != 3 {
                bail!("{cmd} <model_dir> <in.json> <out.json>");
            }
            if cmd == "dump-embed" {
                dump_embed(&dir, &a.pos[1], &a.pos[2], a.device)
            } else {
                dump_rerank(&dir, &a.pos[1], &a.pos[2], a.device)
            }
        }
        other => bail!("unknown command {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(raw: &[&str]) -> Args {
        parse_args(&raw.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()
    }
    #[test]
    fn invalid_options_are_rejected() {
        assert!(parse_args(&["--rounds".into()]).is_err());
        assert!(parse_args(&["--rounds".into(), "-1".into()]).is_err());
        assert!(parse_args(&["--unknown".into()]).is_err());
    }
    #[test]
    fn zero_work_is_rejected_before_loading_weights() {
        let absent = PathBuf::from("unused-model-dir");
        assert!(bench_embed(&absent, &args(&["--rounds", "0"]))
            .unwrap_err()
            .to_string()
            .contains("rounds"));
        assert!(bench_embed(&absent, &args(&["--batch", "0"]))
            .unwrap_err()
            .to_string()
            .contains("batch"));
        assert!(bench_rerank(&absent, &args(&["--docs", "0"]))
            .unwrap_err()
            .to_string()
            .contains("docs"));
        assert!(bench_gemm(&args(&["0"]))
            .unwrap_err()
            .to_string()
            .contains("m > 0"));
    }
}
