//! `agent-memoryd` — 共享记忆库 daemon.
//!
//!   agent-memoryd serve  --data-dir D [--model-provider api|self-hosted] [--embed-url U] [--rerank-url U] [--port 8765] [--prompt-variant zh|en] [--no-reranker]
//!   agent-memoryd mcp    [--url http://127.0.0.1:8765] [--platform claude-code]   (stdio MCP shim, 8 tools)
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
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value_t = 8765)]
        port: u16,
        #[arg(long, env = "MEMORY_PROMPT_VARIANT", default_value = "zh")]
        prompt_variant: String,
        /// Embedding/rerank source: `api` (hosted, key in MEMORY_MODEL_API_KEY or QWEN_API_KEY; default)
        /// or `self-hosted` (user-run llama-server processes)
        #[arg(long, env = "MEMORY_MODEL_PROVIDER")]
        model_provider: Option<String>,
        /// Embedding base URL [default: hosted compatible-mode/v1, or http://127.0.0.1:8081 when self-hosted]
        #[arg(long, env = "MEMORY_EMBED_URL")]
        embed_url: Option<String>,
        /// Rerank base URL [default: hosted compatible-api/v1, or http://127.0.0.1:8082 when self-hosted]
        #[arg(long, env = "MEMORY_RERANK_URL")]
        rerank_url: Option<String>,
        /// Hosted embedding model (api provider) [default: qwen3.7-text-embedding-flash]
        #[arg(long, env = "MEMORY_EMBED_MODEL")]
        embed_model: Option<String>,
        /// Hosted rerank model (api provider) [default: qwen3.7-text-rerank]
        #[arg(long, env = "MEMORY_RERANK_MODEL")]
        rerank_model: Option<String>,
        #[arg(long)]
        no_reranker: bool,
        #[arg(long, env = "LLM_MODEL", default_value = "qwen3.8-flash")]
        llm_model: String,
        #[arg(
            long,
            env = "QWEN_BASE_URL",
            default_value = "https://maas.qianwenaiapi.com/compatible-mode/v1"
        )]
        llm_base_url: String,
        #[arg(long, env = "LLM_MAX_CONCURRENCY", default_value_t = 24)]
        llm_concurrency: usize,
        /// Optional JSON file overriding ServiceConfig fields (camelCase)
        #[arg(long)]
        config: Option<PathBuf>,
        /// Memory algorithm; only mem0 is supported.
        #[arg(long, env = "MEMORY_STRATEGY")]
        memory_strategy: Option<String>,
        #[arg(long, env = "LOG_LEVEL", default_value = "info")]
        log_level: String,
    },
    /// MCP stdio shim exposing eight memory tools; forwards to a running daemon
    Mcp {
        #[arg(long, default_value = "http://127.0.0.1:8765")]
        url: String,
        #[arg(long, default_value = "mcp")]
        platform: String,
    },
    /// Run storage self-tests
    #[cfg(feature = "selftest")]
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
    let cli = Cli::parse();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(16 * 1024 * 1024)
        .build()?;
    rt.block_on(async move {
        match cli.cmd {
            Cmd::Serve {
                data_dir,
                host,
                port,
                prompt_variant,
                model_provider,
                embed_url,
                rerank_url,
                embed_model,
                rerank_model,
                no_reranker,
                llm_model,
                llm_base_url,
                llm_concurrency,
                config,
                memory_strategy,
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
                // Empty values (an unset `.env` template line) leave the JSON/default value in place.
                let given = |v: Option<String>| v.filter(|v| !v.trim().is_empty());
                if let Some(provider) = given(model_provider) {
                    cfg.model_provider = serde_json::from_value(serde_json::json!(provider))?;
                }
                if let Some(url) = given(embed_url) {
                    cfg.embed_url = Some(url);
                }
                if let Some(url) = given(rerank_url) {
                    cfg.rerank_url = Some(url);
                }
                if let Some(model) = given(embed_model) {
                    cfg.embed_model = model;
                }
                if let Some(model) = given(rerank_model) {
                    cfg.rerank_model = model;
                }
                cfg.prompt_variant = prompt_variant;
                anyhow::ensure!(
                    matches!(cfg.prompt_variant.as_str(), "zh" | "en"),
                    "prompt variant must be zh or en"
                );
                cfg.llm_model = llm_model.clone();
                cfg.llm_base_url = llm_base_url.clone();
                cfg.llm_max_concurrency = llm_concurrency;
                if let Some(strategy) = memory_strategy {
                    cfg.memory_strategy = serde_json::from_value(serde_json::json!(strategy))?;
                }
                if no_reranker {
                    cfg.use_reranker = false;
                }
                memory_service::algorithms::validate(&cfg)?;
                let api_key = std::env::var("QWEN_API_KEY").unwrap_or_default();
                let llm = Llm::new(
                    &llm_base_url,
                    &api_key,
                    &llm_model,
                    llm_concurrency,
                    cfg.llm_timeout_secs,
                )?;
                // The retrieval models may use their own key; the generation key is the fallback.
                let model_api_key = std::env::var("MEMORY_MODEL_API_KEY")
                    .ok()
                    .filter(|key| !key.is_empty())
                    .unwrap_or(api_key);
                let models =
                    ModelHub::connect(&cfg, &model_api_key, cfg.use_reranker && cfg.mem0_rerank)
                        .await?;
                let algorithm = memory_service::algorithms::select(cfg.memory_strategy);
                let eng = Engine::new(cfg, llm, models, algorithm);
                // open the default store eagerly so its workers resume unfinished tasks
                eng.tenant("default").await?;
                let app = http::router(eng.clone());
                let listener = tokio::net::TcpListener::bind((host.as_str(), port)).await?;
                tracing::info!("agent-memoryd listening on http://{host}:{port}");
                axum::serve(listener, app).await?;
            }
            #[cfg(feature = "selftest")]
            Cmd::Selftest => {
                memory_service::selftest::store_roundtrip().await;
                memory_service::integrity_tests::selftest().await;
                memory_service::mem0_tests::selftest().await;
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
