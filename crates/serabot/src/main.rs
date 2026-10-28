use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, Subcommand};
use serabot::api::{ApiNotifier, ApiState};
use serabot::config::{Config, load_bots};
use serabot::deck::Brand;
use serabot::documents::index_directory;
use serabot::documents::ingest_file;
use serabot::embeddings::{Embedder, FastEmbedder, LazyEmbedder};
use serabot::error::SeraError;
use serabot::fetch::SafeFetcher;
use serabot::gateway::{Gateway, ToolRegistry};
use serabot::llm::LlmClient;
use serabot::maker::{ArtifactStore, DeckBuildTool, OneDriveUploadTool, TelegramDocumentTool};
use serabot::onedrive::OneDriveClient;
use serabot::policy::Policy;
use serabot::retrieval::{Retriever, SqliteRetriever};
use serabot::runtime::{CompositeNotifier, Notifier, RagSearchTool, Runtime, ScheduledPayload};
use serabot::scheduler::{due_routines, ensure_daily_routine};
use serabot::secrets::{SecretStore, ensure_identity};
use serabot::store::Store;
use serabot::telegram::TelegramClient;
use serabot::tools::{
    DocumentIndexTool, FetchUrlTool, TimesheetTool, WatchNowTool, XlsxAnalyzeTool,
};
use serabot::util::sha256_hex;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Parser)]
#[command(name = "serabot", version, about = "Single-operator self-hosted agent")]
struct Cli {
    #[arg(long, global = true, default_value = "serabot.toml")]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the daemon (worker, Telegram, and owner API as configured).
    Run,
    /// Validate configuration, bots, policy, and database without starting services.
    Check,
    /// Inspect or modify the encrypted secret store.
    Secrets {
        #[command(subcommand)]
        command: SecretsCommand,
    },
    /// Print the SHA-256 hash of an owner API token read from stdin.
    HashToken,
    /// Create a consistent SQLite backup; refuses to overwrite.
    Backup { destination: PathBuf },
    /// Load the local multilingual embedding model and verify it produces vectors.
    SelftestEmbedding,
    /// Ingest a document and run one real vector retrieval against it.
    SelftestRag { path: PathBuf, query: String },
}

#[derive(Debug, Subcommand)]
enum SecretsCommand {
    /// Initialize the age identity and an empty encrypted secret store.
    Init,
    /// Set a secret from stdin without printing it.
    Set { name: String },
    /// List secret names only.
    List,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "serabot=info,tower_http=warn".into()),
        )
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Check => check(&cli.config)?,
        Command::Secrets { command } => match command {
            SecretsCommand::Init => {
                let config = Config::load(&cli.config)?;
                ensure_identity(&config.identity_path())?;
                let store = SecretStore::open(&config)?;
                store.save()?;
                println!(
                    "secret store initialized at {}",
                    config.secrets_path().display()
                );
            }
            SecretsCommand::Set { name } => {
                let config = Config::load(&cli.config)?;
                let mut store = SecretStore::open(&config)?;
                let mut value = String::new();
                std::io::stdin().read_to_string(&mut value)?;
                store.set(&name, value.trim_end_matches(['\r', '\n']))?;
                store.save()?;
                println!("secret '{name}' stored");
            }
            SecretsCommand::List => {
                let config = Config::load(&cli.config)?;
                let store = SecretStore::open(&config)?;
                for name in store.names() {
                    println!("{name}");
                }
            }
        },
        Command::HashToken => {
            let mut token = String::new();
            std::io::stdin().read_to_string(&mut token)?;
            let token = token.trim();
            if token.len() < 32 {
                anyhow::bail!("token must be at least 32 characters");
            }
            println!("{}", sha256_hex(token));
        }
        Command::Run => run(&cli.config).await?,
        Command::Backup { destination } => {
            let config = Config::load(&cli.config)?;
            let store = Store::open(
                config.database_path(),
                config.embeddings.dimension,
                config.database.busy_timeout_ms,
            )?;
            store.backup_to(&destination)?;
            println!("backup written to {}", destination.display());
        }
        Command::SelftestEmbedding => {
            let config = Config::load(&cli.config)?;
            let embedder = FastEmbedder::from_config(&config)?;
            let vectors = embedder
                .embed(vec![
                    "Bonjour, ceci est un test.".to_owned(),
                    "Ciao, questo è un test.".to_owned(),
                ])
                .await?;
            if vectors.len() != 2
                || vectors
                    .iter()
                    .any(|vector| vector.len() != config.embeddings.dimension)
            {
                anyhow::bail!("embedding self-test returned an unexpected shape");
            }
            println!(
                "embedding ok: model={} dimension={} inputs={}",
                embedder.model_id(),
                embedder.dimension(),
                vectors.len()
            );
        }
        Command::SelftestRag { path, query } => {
            let config = Config::load(&cli.config)?;
            let database = std::env::temp_dir()
                .join(format!("serabot-rag-selftest-{}.db", uuid::Uuid::new_v4()));
            let store = Store::open(
                &database,
                config.embeddings.dimension,
                config.database.busy_timeout_ms,
            )?;
            let embedder = FastEmbedder::from_config(&config)?;
            let ingest = ingest_file(&store, &embedder, &path, &config.documents).await?;
            let retriever = SqliteRetriever::new(
                store.clone(),
                Arc::new(embedder),
                config.documents.retrieval_k,
            );
            let chunks = retriever
                .retrieve(&query, config.documents.retrieval_k)
                .await?;
            println!(
                "rag ok: document={} pages={} chunks={} retrieved={}",
                ingest.document_id,
                ingest.pages,
                ingest.chunks,
                chunks.len()
            );
            for chunk in chunks {
                println!(
                    "[doc:{} p.{}] score={:.4} {}",
                    chunk.document_id,
                    chunk.page,
                    chunk.score,
                    chunk.text.chars().take(160).collect::<String>()
                );
            }
        }
    }
    Ok(())
}

fn load(
    config_path: &std::path::Path,
) -> anyhow::Result<(Arc<Config>, Vec<serabot::config::LoadedBot>)> {
    let config = Arc::new(Config::load(config_path)?);
    let bots = load_bots(&config, "bots")?;
    if !bots
        .iter()
        .any(|bot| bot.config.name == "main" && bot.config.enabled)
    {
        anyhow::bail!("an enabled 'main' bot is required");
    }
    Ok((config, bots))
}

fn check(config_path: &std::path::Path) -> anyhow::Result<()> {
    let (config, bots) = load(config_path)?;
    let policy = Policy::load(&config.path(&config.policy.blocklist_file))?;
    let brand = Brand::load(config.path(&config.render.brand_file))?;
    brand.validate()?;
    let store = Store::open(
        config.database_path(),
        config.embeddings.dimension,
        config.database.busy_timeout_ms,
    )?;
    store.health()?;
    for bot in &bots {
        store.upsert_bot(
            &bot.config.name,
            &serde_json::to_value(&bot.config)?,
            bot.config.enabled,
        )?;
    }
    println!(
        "ok: database={}, bots={}, blocklist_terms={}, api={}, telegram={}",
        store.path().display(),
        bots.len(),
        policy.term_count(),
        config.api.enabled,
        config.telegram.enabled
    );
    Ok(())
}

async fn run(config_path: &std::path::Path) -> anyhow::Result<()> {
    let (config, bots) = load(config_path)?;
    let secrets = SecretStore::open(&config).context("open secret store")?;
    let policy = Arc::new(Policy::load(&config.path(&config.policy.blocklist_file))?);
    let store = Store::open(
        config.database_path(),
        config.embeddings.dimension,
        config.database.busy_timeout_ms,
    )?;
    if store.reconcile_dispatching_actions()? > 0 {
        tracing::warn!("reconciled actions with unknown external outcomes");
    }
    for bot in &bots {
        store.upsert_bot(
            &bot.config.name,
            &serde_json::to_value(&bot.config)?,
            bot.config.enabled,
        )?;
    }

    let api_key = secrets
        .require(&config.llm.api_key_secret)
        .map_err(|error| {
            anyhow::anyhow!(
                "{error}; run `serabot secrets set {}` first",
                config.llm.api_key_secret
            )
        })?;
    let llm = LlmClient::new(
        &config.llm.base_url,
        api_key,
        config.llm.timeout_secs,
        config.llm.max_output_tokens,
    )?
    .with_model(&config.llm.model);

    let embedder: Arc<dyn Embedder> = Arc::new(LazyEmbedder::new(
        &config.embeddings,
        config.path(&config.embeddings.cache_dir),
    ));
    let retriever: Arc<dyn Retriever> = Arc::new(SqliteRetriever::new(
        store.clone(),
        embedder.clone(),
        config.documents.retrieval_k,
    ));
    let artifacts = Arc::new(ArtifactStore::new(config.artifacts_dir())?);
    let brand = Arc::new(Brand::load(config.path(&config.render.brand_file))?);
    let mut registry = ToolRegistry::default();
    registry.register(RagSearchTool::new(retriever, 20))?;
    registry.register(DeckBuildTool::new(
        brand,
        artifacts.clone(),
        store.clone(),
        config.render.max_pages,
    ))?;
    registry.register(XlsxAnalyzeTool::new(
        config.path(&config.documents.watch_dir),
    )?)?;
    registry.register(TimesheetTool)?;
    registry.register(DocumentIndexTool::new(
        store.clone(),
        embedder.clone(),
        config.clone(),
    ))?;
    let fetcher = SafeFetcher::new(
        config.watcher.user_agent.clone(),
        config.watcher.timeout_secs,
        config.watcher.max_response_bytes,
    )?;
    let mut source_hosts = std::collections::BTreeSet::new();
    for source in serabot::config::load_sources(&config)? {
        if let Ok(url) = url::Url::parse(&source.url) {
            if let Some(host) = url.host_str() {
                source_hosts.insert(host.to_ascii_lowercase());
                if let Some(stripped) = host.strip_prefix("www.") {
                    source_hosts.insert(stripped.to_ascii_lowercase());
                } else {
                    source_hosts.insert(format!("www.{}", host.to_ascii_lowercase()));
                }
            }
        }
    }
    let fetcher = fetcher.allow_hosts(source_hosts);
    registry.register(FetchUrlTool::new(fetcher.clone()))?;
    registry.register(WatchNowTool::new(store.clone(), fetcher, config.clone()))?;

    let shutdown = CancellationToken::new();
    let mut notifiers: Vec<Arc<dyn Notifier>> = Vec::new();
    let mut telegram: Option<Arc<TelegramClient>> = None;
    if config.telegram.enabled {
        let token = secrets.require(&config.telegram.bot_token_secret)?;
        let client = Arc::new(TelegramClient::new(
            token,
            config.telegram.owner_user_id,
            config.telegram.owner_chat_id,
        )?);
        notifiers.push(client.clone());
        registry.register(TelegramDocumentTool::new(client.clone(), artifacts.clone()))?;
        telegram = Some(client);
    }
    if config.onedrive.enabled {
        let client_id = secrets.require(&config.onedrive.client_id_secret)?;
        let refresh_token = secrets.require(&config.onedrive.refresh_token_secret)?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("build OneDrive HTTP client")?;
        let client = OneDriveClient::new(
            http,
            config.onedrive.graph_base.clone(),
            config.onedrive.tenant.clone(),
            client_id.to_owned(),
            refresh_token.to_owned(),
            config.onedrive.folder.clone(),
            config.onedrive.conflict_behavior.clone(),
        )?;
        registry.register(OneDriveUploadTool::new(
            Arc::new(Mutex::new(client)),
            artifacts.clone(),
            config.onedrive.folder.clone(),
        ))?;
    }
    if config.api.enabled {
        notifiers.push(Arc::new(ApiNotifier::new(store.clone())));
    }
    let notifier = Arc::new(CompositeNotifier::new(notifiers));

    let gateway = Gateway::new(
        store.clone(),
        policy,
        registry,
        config.api.approval_expiry_hours,
        config.runtime.owner_standing_consent,
    );

    let runtime = Runtime::new(
        config.clone(),
        store.clone(),
        gateway,
        llm,
        bots,
        notifier,
        shutdown.clone(),
    )?;

    let worker = tokio::spawn(runtime.clone().run_worker());
    let mut services = Vec::new();

    if config.api.enabled {
        let state = ApiState::new(
            runtime.clone(),
            store.clone(),
            config.clone(),
            artifacts.clone(),
        );
        let app = serabot::api::router(state);
        let listener = tokio::net::TcpListener::bind(&config.server.bind)
            .await
            .with_context(|| format!("bind owner API at {}", config.server.bind))?;
        tracing::info!(bind = %config.server.bind, "owner API listening");
        let shutdown_api = shutdown.clone();
        services.push(tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move { shutdown_api.cancelled().await })
                .await
                .map_err(|error| SeraError::Internal(error.to_string()))
        }));
    }

    if let Some(client) = telegram {
        let runtime = runtime.clone();
        let store = store.clone();
        let artifacts = artifacts.clone();
        let shutdown_telegram = shutdown.clone();
        services.push(tokio::spawn(async move {
            client
                .run_polling(runtime, store, Some(artifacts), shutdown_telegram)
                .await;
            Ok::<(), SeraError>(())
        }));
    }

    {
        let store = store.clone();
        let directory = config.path(&config.documents.watch_dir);
        let documents = config.documents.clone();
        let embedder = embedder.clone();
        let shutdown_index = shutdown.clone();
        services.push(tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                tokio::select! {
                    _ = shutdown_index.cancelled() => break,
                    _ = interval.tick() => {
                        match index_directory(&store, embedder.as_ref(), &directory, &documents).await {
                            Ok(report) => {
                                if !report.ingested.is_empty() || !report.failures.is_empty() {
                                    tracing::info!(
                                        ingested = report.ingested.len(),
                                        failures = report.failures.len(),
                                        "inbox indexing run completed"
                                    );
                                }
                            }
                            Err(error) => tracing::warn!(%error, "inbox indexing failed"),
                        }
                    }
                }
            }
            Ok::<(), SeraError>(())
        }));
    }

    if config.watcher.morning_brief_enabled {
        let tz: chrono_tz::Tz = config
            .schedule
            .timezone
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid schedule.timezone"))?;
        ensure_daily_routine(&store, "morning_brief", &config.schedule.brief_time, tz)?;
        for (index, time) in config.schedule.watch_times.iter().enumerate() {
            ensure_daily_routine(&store, &format!("watch_{index}"), time, tz)?;
        }
        let store = store.clone();
        let shutdown_schedule = shutdown.clone();
        let tick = Duration::from_secs(config.schedule.tick_secs.max(5));
        let timezone = config.schedule.timezone.clone();
        services.push(tokio::spawn(async move {
            let tz: chrono_tz::Tz = timezone
                .parse()
                .map_err(|_| SeraError::Config("invalid schedule timezone".into()))?;
            let mut interval = tokio::time::interval(tick);
            loop {
                tokio::select! {
                    _ = shutdown_schedule.cancelled() => break,
                    _ = interval.tick() => {
                        let due = due_routines(&store, chrono::Utc::now(), tz)?;
                        for routine in due {
                            let kind = if routine.name == "morning_brief" { "brief" } else { "watch" };
                            let payload = ScheduledPayload { conversation_id: "owner".into() };
                            store.enqueue_task(
                                kind,
                                "watcher",
                                &serde_json::to_value(&payload)?,
                                5,
                                chrono::Utc::now(),
                            )?;
                            tracing::info!(routine = %routine.name, local_date = %routine.scheduled_for_local_date, "scheduled task enqueued");
                        }
                    }
                }
            }
            Ok::<(), SeraError>(())
        }));
    }

    tokio::signal::ctrl_c().await?;
    tracing::info!("shutdown requested");
    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(20), worker).await;
    for service in services {
        let _ = tokio::time::timeout(Duration::from_secs(10), service).await;
    }
    Ok(())
}
