//! `agent-memoryd` — 共享记忆库 daemon.
//!
//!   agent-memoryd serve  --data-dir D [--embed-url U] [--rerank-url U] [--port 8765] [--prompt-variant zh|en] [--no-reranker]
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
        /// llama-server base URL serving the embedding model (`--embedding`)
        #[arg(
            long,
            env = "MEMORY_EMBED_URL",
            default_value = "http://127.0.0.1:8081"
        )]
        embed_url: String,
        /// llama-server base URL serving the reranker (`--reranking`)
        #[arg(
            long,
            env = "MEMORY_RERANK_URL",
            default_value = "http://127.0.0.1:8082"
        )]
        rerank_url: String,
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
        /// Select matched formation/retrieval: mem0 (default) or memos (short-term buffers and asynchronous formation).
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
                embed_url,
                rerank_url,
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
                cfg.embed_url = embed_url;
                cfg.rerank_url = rerank_url;
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
                cfg.validate()?;
                let api_key = std::env::var("QWEN_API_KEY").unwrap_or_default();
                let llm = Llm::new(
                    &llm_base_url,
                    &api_key,
                    &llm_model,
                    llm_concurrency,
                    cfg.llm_timeout_secs,
                )?;
                let models = ModelHub::connect(
                    &cfg.embed_url,
                    (cfg.use_reranker && (cfg.memory_strategy == memory_service::config::MemoryStrategy::Memos
                        || cfg.mem0_rerank)).then_some(cfg.rerank_url.as_str()),
                )
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
                memory_service::algorithm_isolation::selftest().await;
                memory_service::selftest::retrieval_roundtrip().await;
                memory_service::selftest::long_turn_roundtrip().await;
                memory_service::algorithms::memos::formation::selftest::formation_selftest().await;
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
