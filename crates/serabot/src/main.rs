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
use serabot::browser::{BrowserToolDeps, VaultResolver, register_browser_tools};
use serabot::config::{Config, load_bots};
use serabot::connectors::register_connector_tools;
use serabot::deck::Brand;
use serabot::documents::index_directory;
use serabot::documents::ingest_file;
use serabot::embeddings::{Embedder, FastEmbedder, LazyEmbedder};
use serabot::error::SeraError;
use serabot::fetch::SafeFetcher;
use serabot::firecrawl::{
    FirecrawlClient, FirecrawlCrawlTool, FirecrawlExtractTool, FirecrawlLimits, FirecrawlMapTool,
    FirecrawlScrapeTool, FirecrawlSearchTool,
};
use serabot::fleet::FleetRegistry;
use serabot::gateway::{Gateway, ToolRegistry};
use serabot::maker::{
    ArtifactStore, DeckBuildTool, OneDriveListTool, OneDriveUploadTool, TelegramDocumentTool,
};
use serabot::openrouter::OpenRouterGenerateImageTool;
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
use serabot::user_secrets::UserSecrets;
use serabot::util::sha256_hex;
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
    /// Replace a user's bot fleet with the templates shipped in this build.
    ///
    /// The database is authoritative after seeding, so a binary upgrade never
    /// silently rewrites a fleet. This command is the explicit opt-in: it drops
    /// the user's `bots` rows and seeds them again from `bots/` + `prompts/`.
    /// Prompt edits, tool allowlists, and any scaffolded bots are lost.
    Fleet {
        #[command(subcommand)]
        command: FleetCommand,
    },
}

#[derive(Debug, Subcommand)]
enum FleetCommand {
    /// Replace every bot row for a user with the shipped templates.
    Reseed {
        /// User id to reseed. Defaults to the bootstrap admin.
        #[arg(long)]
        user: Option<String>,
        /// Print what would change without writing.
        #[arg(long)]
        dry_run: bool,
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
    // 0.28: rolling file logs + a redacting in-memory ring buffer for the
    // console Logs tab, installed before anything logs. Falls back to stdout
    // only when the log directory cannot be created.
    let log_dir = std::env::temp_dir().join("serabot-logs");
    if let Err(error) = serabot::logging::install_default(&log_dir) {
        eprintln!("file logging disabled, stdout only: {error}");
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "serabot=info,tower_http=warn".into()),
            )
            .init();
    }

    let cli = Cli::parse();
    match cli.command {
        Command::Check => check(&cli.config)?,
        Command::Secrets { command } => match command {
            SecretsCommand::Init => {
                let config = Config::load(&cli.config)?;
                ensure_identity(&config.identity_path())?;
                let store = SecretStore::open(&config)?;
                store.save()?;
                // The per-user credential store's master key is generated on
                // first run too, so a fresh deployment has both tiers ready.
                serabot::user_secrets::load_or_create_master_key(
                    &config.path(&config.secrets.master_key_file),
                )?;
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
        Command::Fleet { command } => match command {
            FleetCommand::Reseed { user, dry_run } => {
                let config = Config::load(&cli.config)?;
                let store = Store::open(
                    config.database_path(),
                    config.embeddings.dimension,
                    config.database.busy_timeout_ms,
                )?;
                let user_id = match user {
                    Some(user) => user,
                    None => store.admin_user_id()?,
                };
                if store.get_user(&user_id)?.is_none() {
                    anyhow::bail!("no user with id {user_id}");
                }
                let templates = load_bots(&config, config.path(&config.fleet.bots_dir))?;
                let existing = store.list_bots(&user_id)?;
                println!(
                    "user {user_id}: {} existing bot(s), {} template(s)",
                    existing.len(),
                    templates.len()
                );
                for bot in &templates {
                    let verb = if existing.iter().any(|row| row.name == bot.config.name) {
                        "replace"
                    } else {
                        "add"
                    };
                    println!(
                        "  {verb} {:<12} tools={} skills={}",
                        bot.config.name,
                        bot.config.tools.len(),
                        bot.config.skills.len()
                    );
                }
                for row in &existing {
                    if !templates.iter().any(|bot| bot.config.name == row.name) {
                        println!("  remove {:<12} (not in this build's templates)", row.name);
                    }
                }
                if dry_run {
                    println!("dry run: nothing written");
                } else {
                    let seeded = serabot::fleet::reseed_user(&store, &user_id, &templates)?;
                    println!("reseeded {seeded} bot(s) for {user_id}");
                }
            }
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
            let artifacts = config.artifacts_dir();
            if artifacts.is_dir() {
                let target =
                    std::path::PathBuf::from(format!("{}.artifacts", destination.display()));
                let copied = copy_artifacts(&artifacts, &target)?;
                println!("copied {copied} artifact file(s) to {}", target.display());
            }
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
            let admin = store.admin_user_id()?;
            let ingest = ingest_file(
                &store,
                &admin,
                &embedder,
                &path,
                &config.documents,
                Some(&config),
            )
            .await?;
            let retriever = SqliteRetriever::new(
                store.clone(),
                Arc::new(embedder),
                config.documents.retrieval_k,
            );
            let chunks = retriever
                .retrieve(&admin, &query, config.documents.retrieval_k)
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
    serabot::config::validate_bot_tools(&bots, &serabot::tools::known_tool_names())?;
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

fn copy_artifacts(
    source: &std::path::Path,
    destination: &std::path::Path,
) -> anyhow::Result<usize> {
    std::fs::create_dir_all(destination)?;
    let mut copied = 0usize;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        std::fs::copy(entry.path(), destination.join(entry.file_name()))?;
        copied = copied.saturating_add(1);
    }
    Ok(copied)
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
    let catalog = Arc::new(SkillCatalog::load(&config)?);
    let fleet = FleetRegistry::new(config.clone(), store.clone(), bots, catalog.clone())?;
    let admin_user_id = store.admin_user_id()?;
    fleet.seed(&admin_user_id)?;
    let bot_count = fleet.rows(&admin_user_id)?.len();
    println!(
        "ok: database={}, bots={}, skills={}, blocklist_terms={}, api={}, telegram={}",
        store.path().display(),
        bot_count,
        catalog.skill_ids().len(),
        policy.term_count(),
        config.api.enabled,
        config.telegram.enabled
    );
    Ok(())
}

/// Tell the admin's linked Telegram chat that the service came up. Goes
/// through the same [`Notifier`] path every system message uses (no direct
/// Bot API call), after the same blocklist scan as other outgoing text, and
/// is audited. Best effort: failure is logged and never stops startup.
async fn send_startup_notice(
    notifier: &dyn Notifier,
    policy: &Policy,
    store: &serabot::store::Store,
    admin_user_id: &str,
) {
    let text = format!("SeraBot {} started", env!("CARGO_PKG_VERSION"));
    if !policy.is_allowed(&text) {
        tracing::warn!("startup notice blocked by the content policy");
        return;
    }
    match notifier.send_text(admin_user_id, "system", &text).await {
        Ok(()) => {
            if let Err(error) = store.record_audit(
                admin_user_id,
                "system.started",
                None,
                None,
                None,
                None,
                None,
                Some(&serde_json::json!({"version": env!("CARGO_PKG_VERSION")})),
            ) {
                tracing::warn!(%error, "startup audit write failed");
            }
        }
        Err(error) => tracing::warn!(%error, "startup notice was not delivered"),
    }
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
    // The operator's account id (kv['admin_user_id']) is the owner of the
    // server-level single-operator surfaces that remain in this phase: the
    // configured document inbox, the scheduled routines, and the selftests.
    let admin_user_id = store.admin_user_id()?;
    if store.reconcile_dispatching_actions()? > 0 {
        tracing::warn!("reconciled actions with unknown external outcomes");
    }
    // The repository fleet is the seed source. The admin gets its rows now;
    // members are seeded at account creation and lazily on first fleet use.
    let catalog = Arc::new(SkillCatalog::load(&config)?);
    let fleet = Arc::new(FleetRegistry::new(
        config.clone(),
        store.clone(),
        bots,
        catalog.clone(),
    )?);
    let seeded = fleet.seed(&admin_user_id)?;
    if seeded > 0 {
        tracing::info!(bots = seeded, "seeded the admin fleet from templates");
    }

    // Per-user credential store (phase 4). The operator's existing age-store
    // provider credentials are copied into the admin's rows exactly once;
    // after that every provider client is built from the acting user's
    // credential (own row or an active admin grant).
    let users = Arc::new(UserSecrets::open(&config, store.clone())?);
    let imported = serabot::providers::import_age_provider_credentials(
        &config,
        &secrets,
        &users,
        &store,
        &admin_user_id,
    )?;
    if imported > 0 {
        tracing::info!(
            providers = imported,
            "imported the operator's provider credentials into the admin account"
        );
    }
    // The admin must be able to route the default provider, exactly like the
    // single-operator build required its key at startup. The credential may
    // come from the age store (imported above) or from the console.
    if users.resolve(&admin_user_id, "default")?.is_none() {
        anyhow::bail!(
            "no credential for the default provider; run `serabot secrets set {}` first or set it in the console",
            config.llm.api_key_secret
        );
    }
    let credentials = Arc::new(serabot::providers::UserProviderCredentials::new(
        users.clone(),
        secrets.clone(),
        config.llm.timeout_secs,
        config.llm.max_output_tokens,
    ));
    let router = serabot::model_router::ModelRouter::with_credentials(&config.llm, credentials)?;

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
    serabot::tools::register_workbook_tools(
        &mut registry,
        artifacts.clone(),
        store.clone(),
        config.path(&config.documents.watch_dir),
    )?;
    #[cfg(feature = "docx")]
    serabot::tools::register_docx_tools(
        &mut registry,
        artifacts.clone(),
        store.clone(),
        config.path(&config.documents.watch_dir),
    )?;
    registry.register(DocumentIndexTool::new(
        store.clone(),
        embedder.clone(),
        config.clone(),
    ))?;
    serabot::tools::register_memory_tools(&mut registry, store.clone())?;
    serabot::vault::register_vault_tools(&mut registry, users.clone(), policy.clone())?;
    serabot::cards::register_card_tools(&mut registry, store.clone(), policy.clone())?;
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
    let source_hosts = serabot::sources::watcher_allow_hosts(&config)?;
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
    // Before 0.24 the one ElevenLabs sign-in was server-wide. It moves to the
    // owner's own credential once, and the server-wide names are removed. A
    // failure is logged and boot goes on; the names stay for a later start.
    let migrated = store.admin_user_id().and_then(|owner_id| {
        serabot::elevenlabs_mcp::migrate_server_sign_in(
            &secrets,
            &users,
            &owner_id,
            &config.elevenlabs.oauth_access_token_secret,
            &config.elevenlabs.oauth_refresh_token_secret,
        )
    });
    match migrated {
        Ok(true) => {
            tracing::info!("moved the server-wide ElevenLabs sign-in to the owner's account")
        }
        Ok(false) => {}
        Err(error) => tracing::warn!(
            %error,
            "could not move the server-wide ElevenLabs sign-in; it stays in place until a later start"
        ),
    }
    let elevenlabs = serabot::elevenlabs_mcp::register_elevenlabs_connectors(
        &mut registry,
        config.clone(),
        users.clone(),
        artifacts.clone(),
        policy.clone(),
        store.clone(),
    )?;

    let shutdown = CancellationToken::new();
    let mut notifiers: Vec<Arc<dyn Notifier>> = Vec::new();
    let mut telegram: Option<Arc<TelegramClient>> = None;
    if config.telegram.enabled {
        let token = secrets.require(&config.telegram.bot_token_secret)?;
        let client = Arc::new(
            TelegramClient::new(
                token,
                config.telegram.owner_user_id,
                config.telegram.owner_chat_id,
            )?
            .with_store(store.clone()),
        );
        // The configured owner pair becomes the admin's binding before any
        // notification can be routed; the polling loop repeats this
        // idempotently.
        client.bootstrap_admin_binding(&store)?;
        notifiers.push(client.clone());
        registry.register(TelegramDocumentTool::new(
            client.clone(),
            artifacts.clone(),
            policy.clone(),
        ))?;
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
        registry.register(FirecrawlCrawlTool::new(client.clone(), limits))?;
        registry.register(FirecrawlExtractTool::new(client))?;
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
    // OpenRouter image generation (0.28.0): registered when an OpenRouter
    // provider is configured; the acting user's own OpenRouter sign-in key is
    // used per call. Non-destructive, so no approval is asked.
    if let Some((provider_id, base_url)) = serabot::openrouter::openrouter_provider(&config) {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.llm.timeout_secs.max(30)))
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .context("build OpenRouter image HTTP client")?;
        registry.register(OpenRouterGenerateImageTool::new(
            http,
            users.clone(),
            artifacts.clone(),
            provider_id,
            base_url,
        ))?;
    }
    // Local folder tools relay to the user's desktop; they are inert until a
    // desktop registers an allowlisted folder.
    let local_relay = serabot::local_relay::LocalRelay::new();
    serabot::local_relay::register_local_tools(&mut registry, local_relay.clone(), policy.clone())?;
    serabot::image_ops::register_image_tools(
        &mut registry,
        artifacts.clone(),
        store.clone(),
        config.path(&config.documents.watch_dir),
    )?;
    serabot::local_transfer::register_transfer_tools(
        &mut registry,
        local_relay.clone(),
        policy.clone(),
        artifacts.clone(),
        store.clone(),
        config.path(&config.documents.watch_dir),
    )?;
    // One provider set for the browser fills and the gateway's redactor, so a
    // value a fill reads is scrubbed from what the model sees.
    let providers = Arc::new(serabot::vault_providers::Providers::new(&config.vault));
    let mut browser_manager = None;
    if config.browser.enabled {
        let manager = serabot::browser::build_manager(&config)?;
        register_browser_tools(
            &mut registry,
            BrowserToolDeps {
                manager: manager.clone(),
                resolver: Arc::new(VaultResolver::new(users.clone(), providers.clone())),
                artifacts: Some(artifacts.clone()),
                store: store.clone(),
            },
        )?;
        browser_manager = Some(manager);
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
    let personal_connectors = Arc::new(serabot::personal::PersonalConnectors::new(
        &config,
        &secrets,
        users.clone(),
    )?);
    serabot::personal::register_personal_tools(&mut registry, &config, &personal_connectors)?;
    let composio = Arc::new(serabot::composio::ComposioService::new(
        &config,
        users.clone(),
    )?);
    serabot::composio::register_composio_tools(&mut registry, &composio)?;
    let token_connectors = Arc::new(serabot::token_connectors::TokenConnectors::new(
        &config,
        users.clone(),
    )?);
    serabot::token_connectors::register_token_tools(&mut registry, &token_connectors)?;
    let mut onedrive_registry: Option<Arc<serabot::onedrive::OneDriveRegistry>> = None;
    if config.onedrive.enabled {
        if serabot::onedrive::import_global_refresh_token(
            &config,
            &secrets,
            &users,
            &store,
            &admin_user_id,
        )? {
            tracing::info!(
                "imported the global OneDrive refresh token into the admin credential store"
            );
        }
        let onedrive_clients = Arc::new(serabot::onedrive::OneDriveRegistry::new(
            &config,
            &secrets,
            users.clone(),
        )?);
        registry.register(OneDriveUploadTool::new(
            onedrive_clients.clone(),
            artifacts.clone(),
            config.onedrive.folder.clone(),
        ))?;
        registry.register(OneDriveListTool::new(
            onedrive_clients.clone(),
            config.onedrive.folder.clone(),
        ))?;
        onedrive_registry = Some(onedrive_clients);
    }
    if config.api.enabled {
        notifiers.push(Arc::new(ApiNotifier::new(store.clone())));
    }
    register_skill_tools(
        &mut registry,
        SkillRuntime::new(catalog, fleet.clone(), store.clone()),
    )?;
    registry.register(serabot::delegate::DelegateTool::new(
        store.clone(),
        fleet.clone(),
        router.clone(),
    ))?;
    registry.register(serabot::teams::TeamAskTool::new(
        store.clone(),
        fleet.clone(),
        router.clone(),
    ))?;
    registry.register(serabot::teams::TeamReplyTool::new(
        store.clone(),
        fleet.clone(),
        router.clone(),
        policy.clone(),
    ))?;
    let notifier = Arc::new(CompositeNotifier::new(notifiers));

    let startup_policy = policy.clone();
    let media_policy = policy.clone();
    let gateway = Gateway::new(
        store.clone(),
        policy,
        registry,
        config.api.approval_expiry_hours,
        config.runtime.owner_standing_consent,
    )
    .with_respect_bot_lists(config.approvals.respect_bot_lists)
    .with_vault(users.clone())
    .with_resolved_values(providers.resolved_values());

    let runtime = Runtime::with_fleet(
        config.clone(),
        store.clone(),
        gateway,
        router,
        fleet,
        notifier,
        shutdown.clone(),
    )?;

    let worker = tokio::spawn(runtime.clone().run_worker());
    // Background media jobs (0.24): polls provider renders until they finish.
    let media_jobs = tokio::spawn(serabot::media_jobs::run_media_jobs(
        store.clone(),
        config.artifacts_dir(),
        media_policy,
        shutdown.clone(),
    ));
    let mut services = Vec::new();

    if config.api.enabled {
        let state = ApiState::new(
            runtime.clone(),
            store.clone(),
            config.clone(),
            artifacts.clone(),
            secrets.clone(),
            users.clone(),
            onedrive_registry.clone(),
        )?
        .with_personal(personal_connectors.clone())
        .with_local_relay(local_relay.clone())
        .with_elevenlabs(elevenlabs.clone())
        .with_browser(browser_manager.clone());
        if let Some(manager) = &browser_manager {
            // Closes idle sessions, and every session on shutdown.
            let _reaper = manager.spawn_reaper(shutdown.clone());
        }
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
        let startup_policy = startup_policy.clone();
        let startup_admin = admin_user_id.clone();
        services.push(tokio::spawn(async move {
            // One-line "started" notice through the existing notifier path
            // (admin's linked chat only; blocklist-scanned like any outgoing
            // text). Best effort: a failure is logged, never fatal.
            send_startup_notice(client.as_ref(), &startup_policy, &store, &startup_admin).await;
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
        let admin_index = admin_user_id.clone();
        let shutdown_index = shutdown.clone();
        // The OCR fallback needs the full config (provider + egress consent),
        // so the task carries the Arc rather than only the documents section.
        let config_for_index = config.clone();
        services.push(tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                tokio::select! {
                    _ = shutdown_index.cancelled() => break,
                    _ = interval.tick() => {
                        match index_directory(&store, &admin_index, embedder.as_ref(), &directory, &documents, Some(&config_for_index)).await {
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

    {
        let store = store.clone();
        let config = config.clone();
        let shutdown_reliability = shutdown.clone();
        let tick = Duration::from_secs(config.schedule.tick_secs.max(60));
        services.push(tokio::spawn(async move {
            let mut interval = tokio::time::interval(tick);
            loop {
                tokio::select! {
                    _ = shutdown_reliability.cancelled() => break,
                    _ = interval.tick() => {
                        if let Err(error) = serabot::reliability::enqueue_source_reliability_recoveries(
                            &store,
                            &config,
                            chrono::Utc::now(),
                        ) {
                            tracing::warn!(%error, "reliability enqueue failed");
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
        ensure_daily_routine(
            &store,
            &admin_user_id,
            "morning_brief",
            &config.schedule.brief_time,
            tz,
        )?;
        for (index, time) in config.schedule.watch_times.iter().enumerate() {
            ensure_daily_routine(&store, &admin_user_id, &format!("watch_{index}"), time, tz)?;
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
                        // The scheduler claims routines across users (the
                        // service is shared) and enqueues each occurrence
                        // under the routine's owning user.
                        let due = due_routines(&store, chrono::Utc::now(), tz)?;
                        for routine in due {
                            let kind = serabot::config::task_kind_for_routine(&routine.name);
                            let conversation_id = store
                                .default_scheduled_conversation_id(&routine.user_id)?;
                            let payload = ScheduledPayload {
                                conversation_id,
                                user_id: routine.user_id.clone(),
                            };
                            store.enqueue_task(
                                &routine.user_id,
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
    let _ = tokio::time::timeout(Duration::from_secs(10), media_jobs).await;
    for service in services {
        let _ = tokio::time::timeout(Duration::from_secs(10), service).await;
    }
    Ok(())
}

/// The provider id a CLI login writes its credential under: the configured id
/// when one matches, otherwise the canonical default for the kind.
fn login_provider_id(config: &Config, kind: &str, aliases: &[&str]) -> String {
    config
        .llm
        .providers
        .iter()
        .find(|(id, profile)| aliases.iter().any(|alias| *id == *alias) || profile.kind == kind)
        .map(|(id, _)| id.clone())
        .unwrap_or_else(|| kind.to_owned())
}

/// Store a CLI login's tokens in the admin's per-user credential row.
///
/// Phase 4 moved provider credentials out of the age store: the admin account
/// owns them, rotation writes back there, and the age store keeps only the
/// server-level app registration values.
fn store_admin_provider_login(
    config: &Config,
    provider: &str,
    api_key: &str,
    refresh_token: &str,
) -> anyhow::Result<()> {
    let store = Store::open(
        config.database_path(),
        config.embeddings.dimension,
        config.database.busy_timeout_ms,
    )?;
    let admin = store.admin_user_id()?;
    let users = UserSecrets::open(config, store)?;
    let credential = serabot::user_secrets::ProviderCredential {
        api_key: api_key.to_owned(),
        refresh_token: refresh_token.to_owned(),
        ..Default::default()
    };
    users.set(&admin, provider, &credential.encode()?)?;
    println!("stored the {provider} credential for the admin account");
    Ok(())
}

async fn login_antigravity(config_path: &std::path::Path) -> anyhow::Result<()> {
    use zeroize::Zeroize;
    let config = Config::load(config_path)?;
    let store = SecretStore::open(&config)?;
    let provider_id = login_provider_id(
        &config,
        "google-antigravity",
        &["antigravity", "google-antigravity"],
    );
    let profile = config
        .llm
        .providers
        .iter()
        .find(|(id, _)| id.as_str() == provider_id)
        .map(|(_, profile)| profile);
    let secret_name = profile
        .map(|profile| profile.oauth_client_secret_secret.clone())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "antigravity_oauth_client_secret".into());
    if let Some(id_name) = profile
        .map(|profile| profile.oauth_client_id_secret.clone())
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
    store_admin_provider_login(
        &config,
        &provider_id,
        login.access_token.expose_secret(),
        login.refresh_token.expose_secret(),
    )?;
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
    let provider_id = login_provider_id(&config, "muse-code", &["muse-code"]);
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let credentials = serabot::providers::login_muse_code(&http, |url, code| {
        println!("Open {url}");
        println!("Code: {code}");
    })
    .await?;
    store_admin_provider_login(
        &config,
        &provider_id,
        credentials.api_key.expose_secret(),
        credentials.identity_token.expose_secret(),
    )
}
