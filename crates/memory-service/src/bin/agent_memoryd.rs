//! `agent-memoryd` — 共享记忆库 daemon.
//!
//!   agent-memoryd serve  --data-dir D --models-dir M [--port 8765] [--prompt-variant zh|en] [--no-reranker]
//!   agent-memoryd mcp    [--url http://127.0.0.1:8765] [--platform claude-code]   (stdio MCP shim, 5 ★ tools)
//!   agent-memoryd reindex --vector | --fulltext | --all [--url ...]
use clap::{Parser, Subcommand};
use memory_service::{config::ServiceConfig, http, llm::Llm, models::ModelHub, Engine};
use std::path::PathBuf;

#[cfg(all(not(target_env = "msvc"), not(target_os = "windows")))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Parser)]
#[command(name = "agent-memoryd", about = "Memory memory daemon")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the daemon (HTTP POST /v1/{name})
    Serve {
        #[arg(long, env = "MEMORY_DATA_DIR", default_value = "./memory-data")]
        data_dir: PathBuf,
        #[arg(long, env = "MEMORY_MODELS_DIR", default_value = "./models")]
        models_dir: PathBuf,
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value_t = 8765)]
        port: u16,
        #[arg(long, env = "MEMORY_PROMPT_VARIANT", default_value = "zh")]
        prompt_variant: String,
        /// Embedding model directory name under models-dir
        #[arg(long, default_value = "bge-m3")]
        embed_model: String,
        /// Reranker model directory name under models-dir
        #[arg(long, default_value = "bge-reranker-v2-m3")]
        rerank_model: String,
        /// Local inference device; explicit metal fails if Metal is unavailable.
        #[arg(long, env = "MEMORY_MODEL_DEVICE", default_value = "auto", value_parser = ["auto", "cpu", "metal"])]
        model_device: String,
        #[arg(long)]
        no_reranker: bool,
        #[arg(long, env = "LLM_MODEL", default_value = "deepseek-flash")]
        llm_model: String,
        #[arg(
            long,
            env = "DEEPSEEK_BASE_URL",
            default_value = "https://api.deepseek.com"
        )]
        llm_base_url: String,
        #[arg(long, env = "LLM_MAX_CONCURRENCY", default_value_t = 24)]
        llm_concurrency: usize,
        /// Optional JSON file overriding ServiceConfig fields (camelCase)
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long, env = "LOG_LEVEL", default_value = "info")]
        log_level: String,
    },
    /// MCP stdio shim exposing the 5 ★ tools; forwards to a running daemon
    Mcp {
        #[arg(long, default_value = "http://127.0.0.1:8765")]
        url: String,
        #[arg(long, default_value = "mcp")]
        platform: String,
    },
    /// Run storage self-tests
    Selftest,
    /// Trigger an index rebuild on a running daemon
    Reindex {
        #[arg(long, default_value = "http://127.0.0.1:8765")]
        url: String,
        #[arg(long)]
        vector: bool,
        #[arg(long)]
        fulltext: bool,
        #[arg(long)]
        all: bool,
        #[arg(long, default_value = "default")]
        tenant: String,
    },
}

fn main() -> anyhow::Result<()> {
    for var in ["RAYON_NUM_THREADS", "OMP_NUM_THREADS"] {
        if std::env::var(var).is_err() {
            unsafe { std::env::set_var(var, "2") };
        }
    }
    let cli = Cli::parse();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(16 * 1024 * 1024)
        .build()?;
    rt.block_on(async move {
        match cli.cmd {
            Cmd::Serve {
                data_dir,
                models_dir,
                host,
                port,
                prompt_variant,
                embed_model,
                rerank_model,
                model_device,
                no_reranker,
                llm_model,
                llm_base_url,
                llm_concurrency,
                config,
                log_level,
            } => {
                let bind_address: std::net::IpAddr = host.parse()?;
                anyhow::ensure!(bind_address.is_loopback(), "only loopback binding is supported; remote access requires an authenticated gateway");
                tracing_subscriber::fmt()
                    .with_env_filter(&log_level)
                    .with_writer(std::io::stderr)
                    .init();
                let mut cfg = ServiceConfig::default();
                if let Some(p) = config {
                    let over: serde_json::Value = serde_json::from_slice(&std::fs::read(p)?)?;
                    anyhow::ensure!(over.is_object(), "configuration must be a JSON object");
                    let mut base = serde_json::to_value(&cfg)?;
                    if let (Some(b), Some(o)) = (base.as_object_mut(), over.as_object()) {
                        for (k, v) in o {
                            b.insert(k.clone(), v.clone());
                        }
                    }
                    cfg = serde_json::from_value(base)?;
                }
                cfg.data_dir = data_dir;
                cfg.models_dir = models_dir.clone();
                cfg.prompt_variant = prompt_variant;
                anyhow::ensure!(
                    matches!(cfg.prompt_variant.as_str(), "zh" | "en"),
                    "prompt variant must be zh or en"
                );
                cfg.llm_model = llm_model.clone();
                cfg.llm_base_url = llm_base_url.clone();
                cfg.llm_max_concurrency = llm_concurrency;
                if no_reranker {
                    cfg.use_reranker = false;
                }
                cfg.validate()?;
                let api_key = std::env::var("DEEPSEEK_API_KEY").unwrap_or_default();
                let llm = Llm::new(
                    &llm_base_url,
                    &api_key,
                    &llm_model,
                    llm_concurrency,
                    cfg.llm_timeout_secs,
                )?;
                tracing::info!("loading local models from {}", models_dir.display());
                let rerank_dir = models_dir.join(&rerank_model);
                let models = ModelHub::load_with_device(
                    &models_dir.join(&embed_model),
                    if cfg.use_reranker {
                        Some(rerank_dir.as_path())
                    } else {
                        None
                    },
                    model_device.parse()?,
                )?;
                let eng = Engine::new(cfg, llm, models);
                // open the default store eagerly so its workers resume unfinished tasks
                eng.tenant("default").await?;
                let app = http::router(eng.clone());
                let listener = tokio::net::TcpListener::bind((host.as_str(), port)).await?;
                tracing::info!("agent-memoryd listening on http://{host}:{port}");
                axum::serve(listener, app).await?;
            }
            Cmd::Selftest => {
                memory_service::llm::accounting_selftest();
                memory_service::selftest::store_roundtrip().await;
                memory_service::selftest::retrieval_roundtrip().await;
                memory_service::extract::formation_selftest().await;
                memory_service::consolidate_tests::consolidation_selftest().await;
            }
            Cmd::Mcp { url, platform } => {
                memory_service::mcp::run_stdio(&url, &platform).await?;
            }
            Cmd::Reindex {
                url,
                vector,
                fulltext,
                all,
                tenant,
            } => {
                anyhow::ensure!(vector || fulltext || all, "select --vector, --fulltext or --all");
                let body =
                    serde_json::json!({"vector": vector || all, "fulltext": fulltext || all});
                let resp = reqwest::Client::new()
                    .post(format!("{}/admin/reindex", url.trim_end_matches('/')))
                    .header("x-memory-tenant", tenant)
                    .json(&body)
                    .send()
                    .await?
                    .error_for_status()?;
                println!("{}", resp.text().await?);
            }
        }
        Ok(())
    })
}
