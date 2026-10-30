use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, Subcommand};
use secrecy::ExposeSecret;
use serabot::api::{ApiNotifier, ApiState};
use serabot::audio::{
    AudioTranscribeTool, AudioUnderstandTool, MistralAudioClient, resolve_audio_provider,
};
use serabot::browser::{BrowserSession, register_browser_tools};
use serabot::config::{Config, load_bots};
use serabot::connectors::register_connector_tools;
use serabot::deck::Brand;
use serabot::documents::index_directory;
use serabot::documents::ingest_file;
use serabot::elevenlabs::{ElevenLabsClient, register_elevenlabs_tools};
use serabot::embeddings::{Embedder, FastEmbedder, LazyEmbedder};
use serabot::error::SeraError;
use serabot::fetch::SafeFetcher;
use serabot::firecrawl::{
    FirecrawlClient, FirecrawlCrawlTool, FirecrawlLimits, FirecrawlMapTool, FirecrawlScrapeTool,
    FirecrawlSearchTool,
};
use serabot::gateway::{Gateway, ToolRegistry};
use serabot::llm::LlmClient;
use serabot::maker::{
    ArtifactStore, DeckBuildTool, OneDriveListTool, OneDriveUploadTool, TelegramDocumentTool,
};
use serabot::onedrive::OneDriveClient;
use serabot::policy::Policy;
use serabot::retrieval::{Retriever, SqliteRetriever};
use serabot::runtime::{CompositeNotifier, Notifier, RagSearchTool, Runtime, ScheduledPayload};
use serabot::scheduler::{due_routines, ensure_daily_routine};
use serabot::secrets::{SecretStore, SharedSecrets, ensure_identity};
use serabot::skills::{
    SkillCatalog, SkillRuntime, create_skill, read_skill_body_file, register_skill_tools,
};
use serabot::store::Store;
use serabot::tavily::{TavilyClient, TavilyLimits, TavilySearchTool};
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
    /// Mint, list, or revoke owner API device tokens.
    Token {
        #[command(subcommand)]
        command: TokenCommand,
    },
    /// Create a consistent SQLite backup; refuses to overwrite.
    Backup { destination: PathBuf },
    /// Load the local multilingual embedding model and verify it produces vectors.
    SelftestEmbedding,
    /// Ingest a document and run one real vector retrieval against it.
    SelftestRag { path: PathBuf, query: String },
    /// Create or inspect on-demand skill packs.
    Skill {
        #[command(subcommand)]
        command: SkillCommand,
    },
    /// List builtin provider kinds or sign in to Muse Code.
    Providers {
        #[command(subcommand)]
        command: ProvidersCommand,
    },
}

#[derive(Debug, Subcommand)]
enum SkillCommand {
    /// Scaffold skills/<name>/SKILL.md under the configuration directory.
    Init {
        name: String,
        #[arg(long, default_value = "Operator-defined skill")]
        description: String,
    },
    /// Register a skill pack. Reads markdown instructions from --body-file when set.
    Create {
        name: String,
        #[arg(long, default_value = "Operator-defined skill")]
        description: String,
        /// UTF-8 markdown instructions. Omit to write the standard stub.
        #[arg(long)]
        body_file: Option<PathBuf>,
    },
    /// List skill ids discovered under skills.dir.
    List,
}

#[derive(Debug, Subcommand)]
enum ProvidersCommand {
    /// Print builtin provider kinds. Does not contact a provider.
    Kinds,
    /// Device login for muse-code, or loopback PKCE for google-antigravity.
    Login { kind: String },
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

#[derive(Debug, Subcommand)]
enum TokenCommand {
    /// Mint a device token. Prints it once; only its hash is stored.
    Mint {
        #[arg(long)]
        label: String,
        /// Days until expiry. Omit for no expiry; revocation is the control.
        #[arg(long)]
        days: Option<u32>,
    },
    /// List device tokens. Token hashes are never shown.
    List,
    /// Revoke a device token by id.
    Revoke { id: String },
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
        Command::Token { command } => {
            let config = Config::load(&cli.config)?;
            let store = Store::open(
                config.database_path(),
                config.embeddings.dimension,
                config.database.busy_timeout_ms,
            )?;
            match command {
                TokenCommand::Mint { label, days } => {
                    let token = serabot::token_cli::mint(&store, &label, days)?;
                    eprintln!("store this token now; the server keeps only its hash");
                    println!("{token}");
                }
                TokenCommand::List => {
                    for token in serabot::token_cli::list(&store)? {
                        println!("{}", serabot::token_cli::format_line(&token));
                    }
                }
                TokenCommand::Revoke { id } => {
                    if !serabot::token_cli::revoke(&store, &id)? {
                        anyhow::bail!("no device token with id {id}");
                    }
                    println!("revoked {id}");
                }
            }
        }
        Command::Providers { command } => match command {
            ProvidersCommand::Kinds => {
                for item in serabot::providers::builtins() {
                    println!(
                        "{}\t{}\t{}",
                        item.kind,
                        item.base_url.unwrap_or("-"),
                        item.summary
                    );
                }
            }
            ProvidersCommand::Login { kind } => match kind.as_str() {
                "muse-code" => login_muse(&cli.config).await?,
                "google-antigravity" => login_antigravity(&cli.config).await?,
                _ => anyhow::bail!(
                    "provider login is implemented for muse-code and google-antigravity"
                ),
            },
        },
        Command::Skill { command } => match command {
            SkillCommand::Init { name, description } => {
                register_skill_pack(&cli.config, &name, &description, None)?;
            }
            SkillCommand::Create {
                name,
                description,
                body_file,
            } => {
                register_skill_pack(&cli.config, &name, &description, body_file.as_deref())?;
            }
            SkillCommand::List => {
                let config = Config::load(&cli.config)?;
                let catalog = SkillCatalog::load(&config)?;
                for summary in catalog.summaries() {
                    if let Some(pack) = &summary.pack {
                        println!(
                            "{}\t{}\tpack={}\t{}",
                            summary.id, summary.name, pack, summary.description
                        );
                    } else {
                        println!("{}\t{}\t{}", summary.id, summary.name, summary.description);
                    }
                }
            }
        },
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
    let bots = load_bots(&config, &config.fleet.bots_dir)?;
    if !bots
        .iter()
        .any(|bot| bot.config.name == "main" && bot.config.enabled)
    {
        anyhow::bail!("an enabled 'main' bot is required");
    }
    SkillCatalog::load(&config)?.validate_bot_skills(&bots)?;
    Ok((config, bots))
}

fn register_skill_pack(
    config_path: &std::path::Path,
    name: &str,
    description: &str,
    body_file: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    let config = Config::load(config_path)?;
    let policy = Policy::load(&config.path(&config.policy.blocklist_file))?;
    let body = match body_file {
        Some(path) => Some(read_skill_body_file(path)?),
        None => None,
    };
    let created = create_skill(&config, name, description, body.as_deref(), &policy)?;
    println!("skill pack written to {}", created.file);
    Ok(())
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
    let catalog = SkillCatalog::load(&config)?;
    println!(
        "ok: database={}, bots={}, skills={}, blocklist_terms={}, api={}, telegram={}",
        store.path().display(),
        bots.len(),
        catalog.skill_ids().len(),
        policy.term_count(),
        config.api.enabled,
        config.telegram.enabled
    );
    Ok(())
}

async fn run(config_path: &std::path::Path) -> anyhow::Result<()> {
    let (config, bots) = load(config_path)?;
    let secrets = SharedSecrets::new(SecretStore::open(&config).context("open secret store")?);
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

    secrets
        .require(&config.llm.api_key_secret)
        .map_err(|error| {
            anyhow::anyhow!(
                "{error}; run `serabot secrets set {}` first",
                config.llm.api_key_secret
            )
        })?;
    let llm = open_provider_client(
        &config.llm.default_provider(),
        &secrets,
        config.llm.timeout_secs,
        config.llm.max_output_tokens,
    )?
    .with_model(&config.llm.model);
    let mut clients = std::collections::BTreeMap::from([("default".to_owned(), llm)]);
    for (id, provider) in &config.llm.providers {
        clients.insert(
            id.clone(),
            open_provider_client(
                provider,
                &secrets,
                config.llm.timeout_secs,
                config.llm.max_output_tokens,
            )?,
        );
    }
    let router = serabot::model_router::ModelRouter::new(&config.llm, clients)?;

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
    serabot::tools::register_workbook_tools(&mut registry, artifacts.clone(), store.clone())?;
    registry.register(DocumentIndexTool::new(
        store.clone(),
        embedder.clone(),
        config.clone(),
    ))?;
    serabot::tools::register_memory_tools(&mut registry, store.clone())?;
    serabot::tools::register_pdf_tools(
        &mut registry,
        artifacts.clone(),
        store.clone(),
        config.path(&config.documents.watch_dir),
    )?;
    #[cfg(feature = "pptx")]
    serabot::tools::register_pptx_tools(
        &mut registry,
        artifacts.clone(),
        store.clone(),
        config.path(&config.documents.watch_dir),
    )?;
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
    if config.audio.enabled {
        let provider = resolve_audio_provider(&config.llm, &config.audio)?;
        let api_key = secrets.require(&provider.api_key_secret).map_err(|error| {
            anyhow::anyhow!(
                "{error}; run `serabot secrets set {}` before enabling audio",
                provider.api_key_secret
            )
        })?;
        let audio_client = Arc::new(MistralAudioClient::new(
            provider.base_url,
            api_key,
            config.llm.timeout_secs,
            &config.audio,
            provider.data_egress_approved,
        )?);
        registry.register(AudioTranscribeTool::new(
            audio_client.clone(),
            config.clone(),
        ))?;
        registry.register(AudioUnderstandTool::new(audio_client, config.clone()))?;
    }
    if config.elevenlabs.enabled {
        let api_key = secrets
            .require(&config.elevenlabs.api_key_secret)
            .map_err(|error| {
                anyhow::anyhow!(
                    "{error}; run `serabot secrets set {}` before enabling ElevenLabs",
                    config.elevenlabs.api_key_secret
                )
            })?;
        let elevenlabs = Arc::new(ElevenLabsClient::new(
            &config.elevenlabs.api_base,
            api_key,
            config.elevenlabs.timeout_secs,
        )?);
        register_elevenlabs_tools(
            &mut registry,
            elevenlabs,
            artifacts.clone(),
            policy.clone(),
            config.clone(),
            config.elevenlabs.clone(),
        )?;
    }

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
    if config.firecrawl.enabled {
        let api_key = secrets
            .require(&config.firecrawl.api_key_secret)
            .map_err(|error| {
                anyhow::anyhow!(
                    "{error}; run `serabot secrets set {}` first",
                    config.firecrawl.api_key_secret
                )
            })?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.firecrawl.timeout_secs.max(5)))
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .context("build Firecrawl HTTP client")?;
        let client = Arc::new(FirecrawlClient::new(
            http,
            config.firecrawl.api_base.clone(),
            api_key,
            &config.firecrawl,
        )?);
        let limits = FirecrawlLimits::from_config(&config.firecrawl);
        registry.register(FirecrawlScrapeTool::new(client.clone(), limits.clone()))?;
        registry.register(FirecrawlMapTool::new(client.clone(), limits.clone()))?;
        registry.register(FirecrawlSearchTool::new(client.clone(), limits.clone()))?;
        registry.register(FirecrawlCrawlTool::new(client, limits))?;
    }
    if config.tavily.enabled {
        let api_key = secrets
            .require(&config.tavily.api_key_secret)
            .map_err(|error| {
                anyhow::anyhow!(
                    "{error}; run `serabot secrets set {}` first",
                    config.tavily.api_key_secret
                )
            })?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.tavily.timeout_secs.max(5)))
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .context("build Tavily HTTP client")?;
        let client = Arc::new(TavilyClient::new(
            http,
            config.tavily.api_base.clone(),
            api_key,
        )?);
        registry.register(TavilySearchTool::new(
            client,
            TavilyLimits::from_config(&config.tavily),
        ))?;
    }
    if config.browser.enabled {
        let session = Arc::new(BrowserSession::new(config.browser.clone())?);
        register_browser_tools(&mut registry, session)?;
    }
    if config.connectors.any_enabled() {
        let connector_http = reqwest::Client::builder()
            .timeout(Duration::from_secs(
                config.connectors.agentmail.timeout_secs.max(30),
            ))
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .context("build connector HTTP client")?;
        register_connector_tools(&mut registry, &config.connectors, &secrets, connector_http)?;
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
        )?
        .with_refresh_persistence(
            secrets.clone(),
            config.onedrive.refresh_token_secret.clone(),
            None,
        )?;
        let client = Arc::new(Mutex::new(client));
        registry.register(OneDriveUploadTool::new(
            client.clone(),
            artifacts.clone(),
            config.onedrive.folder.clone(),
        ))?;
        registry.register(OneDriveListTool::new(
            client,
            config.onedrive.folder.clone(),
        ))?;
    }
    if config.api.enabled {
        notifiers.push(Arc::new(ApiNotifier::new(store.clone())));
    }
    let catalog = Arc::new(SkillCatalog::load(&config)?);
    register_skill_tools(&mut registry, SkillRuntime::new(catalog, &bots))?;
    let bot_map: Arc<std::collections::HashMap<String, serabot::config::LoadedBot>> = Arc::new(
        bots.iter()
            .cloned()
            .map(|bot| (bot.config.name.clone(), bot))
            .collect(),
    );
    registry.register(serabot::delegate::DelegateTool::new(
        store.clone(),
        bot_map.clone(),
        router.clone(),
    ))?;
    registry.register(serabot::teams::TeamAskTool::new(
        store.clone(),
        bot_map.clone(),
        router.clone(),
    ))?;
    registry.register(serabot::teams::TeamReplyTool::new(
        store.clone(),
        bot_map,
        router.clone(),
        policy.clone(),
    ))?;
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
        router,
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
            secrets.clone(),
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

fn muse_secret_names(config: &Config) -> (String, String) {
    let profile = config
        .llm
        .providers
        .iter()
        .find(|(id, profile)| *id == "muse-code" || profile.kind == "muse-code")
        .map(|(_, profile)| profile);
    let api = profile
        .map(|profile| profile.api_key_secret.clone())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "muse_code_api_key".into());
    let identity = profile
        .map(|profile| profile.refresh_token_secret.clone())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "muse_code_identity".into());
    (api, identity)
}

fn open_provider_client(
    profile: &serabot::config::ProviderConfig,
    secrets: &SharedSecrets,
    timeout_secs: u64,
    max_output_tokens: u32,
) -> anyhow::Result<LlmClient> {
    use serabot::providers::{AuthPlan, OAuthMaterial, OAuthStyle, auth_plan, resolve_base_url};
    let endpoint = resolve_base_url(&profile.kind, &profile.base_url)?;
    let api_key = secrets.require(&profile.api_key_secret)?.to_owned();
    let mut client = LlmClient::new(&endpoint, api_key, timeout_secs, max_output_tokens)?;
    let plan = auth_plan(&profile.kind, &profile.auth)?;
    if let AuthPlan::OAuth {
        style,
        token_url,
        client_id_fallback,
    } = plan
    {
        if !profile.refresh_token_secret.trim().is_empty() {
            let refresh_token = secrets.require(&profile.refresh_token_secret)?.to_owned();
            let client_id = if profile.oauth_client_id_secret.trim().is_empty() {
                client_id_fallback.unwrap_or("").to_owned()
            } else {
                secrets.require(&profile.oauth_client_id_secret)?.to_owned()
            };
            let client_secret = if profile.oauth_client_secret_secret.trim().is_empty() {
                String::new()
            } else {
                secrets
                    .require(&profile.oauth_client_secret_secret)?
                    .to_owned()
            };
            if style == OAuthStyle::RefreshToken && client_secret.is_empty() {
                anyhow::bail!("oauth client secret is required to refresh this provider");
            }
            client = client.with_oauth(OAuthMaterial {
                style,
                refresh_token,
                token_url: token_url.to_owned(),
                client_id,
                client_secret,
            })?;
            if style == OAuthStyle::RefreshToken {
                client = client.with_refresh_persistence(
                    secrets.clone(),
                    profile.refresh_token_secret.clone(),
                    profile.api_key_secret.clone(),
                )?;
            }
        }
    }
    if serabot::providers::canonical_kind(&profile.kind)? == "google-antigravity" {
        client = client.with_cloud_code(&profile.project_id)?;
    }
    Ok(client)
}

async fn login_antigravity(config_path: &std::path::Path) -> anyhow::Result<()> {
    use zeroize::Zeroize;
    let config = Config::load(config_path)?;
    let mut store = SecretStore::open(&config)?;
    let profile = config.llm.providers.iter().find(|(id, profile)| {
        *id == "antigravity" || *id == "google-antigravity" || profile.kind == "google-antigravity"
    });
    let api_name = profile
        .map(|(_, profile)| profile.api_key_secret.clone())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "antigravity_access_token".into());
    let refresh_name = profile
        .map(|(_, profile)| profile.refresh_token_secret.clone())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "antigravity_refresh_token".into());
    let secret_name = profile
        .map(|(_, profile)| profile.oauth_client_secret_secret.clone())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "antigravity_oauth_client_secret".into());
    if let Some(id_name) = profile
        .map(|(_, profile)| profile.oauth_client_id_secret.clone())
        .filter(|name| !name.trim().is_empty())
    {
        let stored = store.require(&id_name)?;
        if stored != serabot::providers::ANTIGRAVITY_CLIENT_ID {
            anyhow::bail!("oauth client id does not match the Antigravity client");
        }
    }
    let mut client_secret = store.require(&secret_name)?.to_owned();
    let login = serabot::cloud_code::login_antigravity(&client_secret, |url| {
        println!("Open {url}");
    })
    .await;
    client_secret.zeroize();
    let login = login?;
    store.set(&api_name, login.access_token.expose_secret())?;
    store.set(&refresh_name, login.refresh_token.expose_secret())?;
    store.save()?;
    println!("stored secret names {api_name} and {refresh_name}");
    match login.project_id.as_deref() {
        Some(project) => {
            println!("project_id={project}");
            println!(
                "Set project_id on the google-antigravity provider. An empty project_id omits the field."
            );
        }
        None => println!(
            "No Cloud Code project was provisioned. Chat and catalog omit project until project_id is set."
        ),
    }
    Ok(())
}

async fn login_muse(config_path: &std::path::Path) -> anyhow::Result<()> {
    let config = Config::load(config_path)?;
    let mut store = SecretStore::open(&config)?;
    let (api_name, identity_name) = muse_secret_names(&config);
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let credentials = serabot::providers::login_muse_code(&http, |url, code| {
        println!("Open {url}");
        println!("Code: {code}");
    })
    .await?;
    store.set(&api_name, credentials.api_key.expose_secret())?;
    store.set(&identity_name, credentials.identity_token.expose_secret())?;
    store.save()?;
    println!("stored secret names {api_name} and {identity_name}");
    Ok(())
}
