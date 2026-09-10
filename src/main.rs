mod chronicle;
mod database;
mod discord;
mod jester;
mod utils;

use std::path::PathBuf;

////////////////////////////////////////////////////////////////////////////////
use dotenv::from_path;
/// Imports
use poise::serenity_prelude::{ClientBuilder, Context as SerenityContext, GatewayIntents};
use serenity::client::FullEvent;
use songbird::{
    Config as SongbirdConfig, SerenityInit,
    driver::{DecodeConfig, DecodeMode},
};
use sqlx::SqlitePool;
use tracing::info;

use crate::{
    chronicle::{
        config::Config,
        indexer::{db::repository::facade::IndexerDb, embedder::Embedder, service::Indexer},
        llm::Llm,
        recording::recorder::{notify_recording_user, scan_incomplete_manifests},
        runtime::GpuRuntime,
        service::Chronicle,
    },
    discord::context::{Data, Error},
    jester::library::sync::sync_audio_library,
};
use anyhow::{Context, Result, bail};
use chrono::Utc;
use std::fs::{self, OpenOptions};
use std::path::Path;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

const DEFAULT_CHRONICLE_EVAL_SUITE: &str = "tests/fixtures/chronicle/suite.toml";
const DEFAULT_CHRONICLE_QUERY_EVAL_SUITE: &str = "tests/fixtures/chronicle-query/suite.toml";
const DEFAULT_CHRONICLE_SYNTHESIS_EVAL_SUITE: &str =
    "tests/fixtures/chronicle-synthesis/suite.toml";
const DEFAULT_LOG_LEVEL: &str = "info";

////////////////////////////////////////////////////////////////////////////////
// Functions

async fn on_error(error: poise::FrameworkError<'_, Data, Error>) {
    // 1) Inspect & log any command errors without moving out of `error`
    match &error {
        // Log setup failures before forwarding them to Poise's handler.
        poise::FrameworkError::Setup {
            error: setup_err, ..
        } => {
            tracing::error!(error = ?setup_err, "Failed to start bot");
        }
        // Log command errors
        poise::FrameworkError::Command {
            ctx,
            error: cmd_err,
            ..
        } => {
            tracing::warn!(command = %ctx.command().name, error = ?cmd_err, "Command failed");
        }
        // You can match other variants here if you like...
        _ => {}
    }

    // 2) Forward the _owned_ `error` to Poise's default handler so it replies in Discord
    if let Err(e) = poise::builtins::on_error(error).await {
        tracing::error!("Error while handling error: {}", e);
    }
}

async fn build_chronicle(config: &Config) -> Result<Chronicle> {
    tracing::info!(
        chronicle_db = %config.database.chronicle,
        "Opening Chronicle index database"
    );

    let chronicle_db = IndexerDb::open(&config.database.chronicle)
        .await
        .context("Failed to open Chronicle index database")?;
    tracing::info!("Loading Chronicle embedding model");
    let device = candle_core::Device::cuda_if_available(0)
        .context("Failed to select a CUDA or CPU device for Chronicle embeddings")?;
    let embedder =
        Embedder::load(device).context("Failed to load the Chronicle embedding model")?;
    let indexer = Indexer::new(
        PathBuf::from(&config.chronicle.corpus_dir),
        chronicle_db,
        embedder,
        config.chronicle.max_chunk_tokens,
        config.chronicle.chunk_overlap_tokens,
    )
    .with_excluded_note_ids(config.chronicle.excluded_note_ids.clone());

    let indexing_stats = indexer
        .index()
        .await
        .context("Failed to index the Chronicle corpus")?;
    tracing::info!(
        added = indexing_stats.added,
        updated = indexing_stats.updated,
        unchanged = indexing_stats.unchanged,
        removed = indexing_stats.removed,
        "Chronicle index complete"
    );

    let (chronicle_db, _embedder) = indexer.into_parts();
    let runtime = GpuRuntime::new();
    let llm = Llm::new(&config.chronicle, runtime.clone());
    tracing::info!("Chronicle services initialized");

    Ok(Chronicle::new(
        chronicle_db,
        llm,
        runtime,
        config.chronicle.retrieval_limit,
        config.chronicle.retrieval_candidate_limit,
        config.chronicle.retrieval_distance_threshold,
        config.chronicle.retrieval_near_duplicate_threshold,
        config.chronicle.retrieval_max_chunks_per_document,
        config.chronicle.pagerank_weight,
        config.chronicle.synthesis,
        config.chronicle.llm_max_reply_length,
    ))
}

fn build_commands() -> Vec<poise::Command<Data, Error>> {
    vec![
        discord::commands::admin::help(),
        discord::commands::admin::register(),
        discord::commands::controls::join(),
        discord::commands::controls::play(),
        discord::commands::controls::mix(),
        discord::commands::controls::queue(),
        discord::commands::controls::skip(),
        discord::commands::controls::history(),
        discord::commands::controls::leave(),
        discord::commands::controls::loop_track(),
        discord::commands::controls::pause(),
        discord::commands::controls::now_playing(),
        discord::commands::management::download(),
        discord::commands::management::reset_taxonomy(),
        discord::commands::management::set_taxonomy(),
        discord::commands::management::add_texture(),
        discord::commands::management::add_environment(),
        discord::commands::management::add_label(),
        discord::commands::management::set_metadata(),
        discord::commands::management::fix(),
        discord::commands::library::library(),
        discord::commands::chronicle::recording(),
        discord::commands::chronicle::transcript(),
        discord::commands::chronicle::chronicle(),
    ]
}

fn handle_event<'a>(
    ctx: &'a SerenityContext,
    event: &'a FullEvent,
    _framework: poise::FrameworkContext<'a, Data, Error>,
    data: &'a Data,
) -> poise::BoxFuture<'a, Result<(), Error>> {
    Box::pin(async move {
        if let FullEvent::VoiceStateUpdate { old, new } = event {
            let Some(guild_id) = new.guild_id else {
                return Ok(());
            };

            let Some(recorder) = data.recorder.get(guild_id).await else {
                return Ok(());
            };

            let Some((voice_channel_id, notification_channel_id, initiator)) =
                recorder.recording_info().await
            else {
                return Ok(());
            };

            let user_id = new.user_id;
            tracing::debug!(
                ?guild_id,
                ?user_id,
                ?old,
                ?new,
                "Received voice state update"
            );

            // The initiator is deliberately excluded from notifications.
            if user_id == initiator {
                return Ok(());
            }

            // Only notify when the user enters the recording channel.
            if new.channel_id != Some(voice_channel_id)
                || old.as_ref().and_then(|state| state.channel_id) == Some(voice_channel_id)
            {
                return Ok(());
            }

            notify_recording_user(&ctx.http, notification_channel_id, user_id).await?;
            tracing::info!(?guild_id, ?user_id, "Notified user about active recording");
        }

        Ok(())
    })
}

fn build_framework(
    pool: SqlitePool,
    config: Config,
    chronicle: Chronicle,
    poise_commands: Vec<poise::Command<Data, Error>>,
) -> poise::Framework<Data, Error> {
    let poise_options = poise::FrameworkOptions {
        commands: poise_commands,
        prefix_options: poise::PrefixFrameworkOptions {
            prefix: Some(">".into()),
            ..Default::default()
        },
        on_error: |error| Box::pin(on_error(error)),
        pre_command: |ctx| {
            Box::pin(async move {
                tracing::debug!("Executing command {}...", ctx.command().qualified_name);
            })
        },
        post_command: |ctx| {
            Box::pin(async move {
                tracing::debug!(
                    "Successfully executed command {}",
                    ctx.command().qualified_name
                );
            })
        },
        skip_checks_for_owners: true,
        event_handler: handle_event,
        ..Default::default()
    };

    poise::Framework::builder()
        .options(poise_options)
        .setup(|_ctx, _ready, _framework| {
            Box::pin(async move { Ok(Data::new(pool, config, chronicle)) })
        })
        .build()
}

#[tokio::main]
async fn main() {
    let project_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let env_path = project_root.join(".env");

    // Load .env before constructing the logging filter. RUST_LOG in .env is
    // intentionally authoritative, even if the process inherited another
    // value from its shell environment.
    #[allow(deprecated)]
    let dotenv_entries = dotenv::from_path_iter(&env_path);
    if let Ok(entries) = dotenv_entries {
        for entry in entries.flatten() {
            if entry.0 == "RUST_LOG" {
                // SAFETY: this runs before Tokio starts any application tasks.
                unsafe { std::env::set_var(entry.0, entry.1) };
            }
        }
    }
    from_path(&env_path).ok();

    // Keep normal operation useful without being noisy. RUST_LOG is read from
    // .env above, for example: `RUST_LOG=chester_rs=debug`.
    let (env_filter, invalid_filter) = match EnvFilter::try_from_default_env() {
        Ok(filter) => (filter, None),
        Err(error) => (EnvFilter::new("chester_rs=info,warn"), Some(error)),
    };

    let log_directory = project_root.join("logs/application");
    #[allow(clippy::print_stderr)]
    let log_file = match create_log_file(&log_directory) {
        Ok(file) => file,
        Err(error) => {
            eprintln!("Failed to initialize logfile: {error:#}");
            std::process::exit(1);
        }
    };
    let terminal_layer = tracing_subscriber::fmt::layer();
    let file_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(log_file);

    #[allow(clippy::print_stderr)]
    if let Err(error) = tracing_subscriber::registry()
        .with(env_filter)
        .with(terminal_layer)
        .with(file_layer)
        .try_init()
    {
        eprintln!("Failed to initialize logging: {error}");
        std::process::exit(1);
    }

    if let Some(error) = invalid_filter {
        tracing::warn!(?error, "Invalid RUST_LOG filter; using the default filter");
    }

    if let Err(error) = run().await {
        tracing::error!("Chester failed to start: {error:#}");
        tracing::debug!(error = ?error, "Startup error chain");
        std::process::exit(1);
    }
}

fn create_log_file(directory: &Path) -> Result<std::fs::File> {
    fs::create_dir_all(directory).with_context(|| {
        format!(
            "Failed to create logfile directory at {}",
            directory.display()
        )
    })?;
    let timestamp = Utc::now().format("%Y%m%d-%H%M%S");
    let level = configured_log_level();
    let version = env!("CARGO_PKG_VERSION");
    let mut suffix = 0_u64;
    loop {
        let filename = if suffix == 0 {
            format!("chester-{timestamp}-{level}-v{version}.log")
        } else {
            format!("chester-{timestamp}-{level}-v{version}-{suffix}.log")
        };
        let path = directory.join(filename);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok(file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => suffix += 1,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("Failed to create logfile at {}", path.display()));
            }
        }
    }
}

fn configured_log_level() -> &'static str {
    let settings = std::env::var("RUST_LOG").unwrap_or_default().to_lowercase();
    ["trace", "debug", "info", "warn", "error"]
        .into_iter()
        .find(|level| {
            settings
                .split(|c: char| !c.is_ascii_alphabetic())
                .any(|part| part == *level)
        })
        .unwrap_or(DEFAULT_LOG_LEVEL)
}

async fn run_evaluation_command() -> Result<bool> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments
        .first()
        .is_some_and(|arg| arg == "--chronicle-synthesis-eval")
    {
        anyhow::ensure!(
            arguments.len() <= 3,
            "Usage: chester-rs --chronicle-synthesis-eval [SUITE.toml] [REPORT.json]"
        );
        let suite_path = arguments
            .get(1)
            .map_or(DEFAULT_CHRONICLE_SYNTHESIS_EVAL_SUITE, String::as_str);
        chronicle::synthesis_eval::run(
            std::path::Path::new(suite_path),
            arguments.get(2).map(std::path::Path::new),
        )
        .await?;
        return Ok(true);
    }
    if arguments
        .first()
        .is_some_and(|arg| arg == "--chronicle-query-eval")
    {
        let mut args = arguments.iter().skip(1).collect::<Vec<_>>();
        let test_planner = args.last().is_some_and(|arg| arg.as_str() == "--planner");
        if test_planner {
            args.pop();
        }
        anyhow::ensure!(
            args.len() <= 2,
            "Usage: chester-rs --chronicle-query-eval [SUITE.toml] [REPORT.json] [--planner]"
        );
        let suite_path = args
            .first()
            .map_or(DEFAULT_CHRONICLE_QUERY_EVAL_SUITE, |path| path.as_str());
        chronicle::query::eval::run(
            std::path::Path::new(suite_path),
            args.get(1).map(std::path::Path::new),
            test_planner,
        )
        .await?;
        return Ok(true);
    }
    if arguments
        .first()
        .is_some_and(|arg| arg == "--chronicle-eval")
    {
        anyhow::ensure!(
            (1..=3).contains(&arguments.len()),
            "Usage: chester-rs --chronicle-eval [SUITE.toml] [REPORT.json]"
        );
        let suite_path = arguments
            .get(1)
            .map_or(DEFAULT_CHRONICLE_EVAL_SUITE, String::as_str);
        chronicle::eval::run(
            std::path::Path::new(suite_path),
            arguments.get(2).map(std::path::Path::new),
        )
        .await?;
        return Ok(true);
    }
    Ok(false)
}

async fn run() -> Result<()> {
    if run_evaluation_command().await? {
        return Ok(());
    }
    let project_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));

    tracing::info!("Starting Chester");

    let config_path = project_root.join(".chronicle/config.toml");
    let config = Config::load(&config_path).with_context(|| {
        format!(
            "Failed to load configuration from {}",
            config_path.display()
        )
    })?;
    tracing::debug!(
        corpus_dir = %config.chronicle.corpus_dir,
        retrieval_limit = config.chronicle.retrieval_limit,
        max_chunk_tokens = config.chronicle.max_chunk_tokens,
        max_reply_length = config.chronicle.llm_max_reply_length,
        "Loaded configuration"
    );

    scan_incomplete_manifests(&config.paths.recordings_dir)
        .context("Failed to scan recording manifests")?;

    let token = std::env::var("DISCORD_TOKEN").context("DISCORD_TOKEN is not set")?;

    let pool = database::pool::open_sqlite_pool(&config.database.jester, "Jester")
        .await
        .context("Failed to open the Jester database")?;
    jester::db::schema::initialise(&pool)
        .await
        .context("Failed to initialize the Jester database schema")?;
    let chronicle = build_chronicle(&config)
        .await
        .context("Failed to initialize Chronicle")?;

    let sync_stats = sync_audio_library(&pool)
        .await
        .context("Failed to synchronize the audio library")?;

    info!(
        downloaded = sync_stats.downloaded,
        failed = sync_stats.failed,
        skipped = sync_stats.skipped,
        "Library sync complete"
    );

    if sync_stats.failed > 0 {
        bail!(
            "Audio library synchronization failed for {} track(s); refusing to start",
            sync_stats.failed
        );
    }

    let poise_commands = build_commands();
    tracing::info!(
        command_count = poise_commands.len(),
        "Registering bot commands"
    );

    let framework = build_framework(pool, config, chronicle, poise_commands);

    let intents = GatewayIntents::non_privileged() | GatewayIntents::MESSAGE_CONTENT;

    // 2) Build the Songbird config too (required for decoding voice data)
    let songbird_config =
        SongbirdConfig::default().decode_mode(DecodeMode::Decode(DecodeConfig::default()));

    // 3) Create the Serenity client, attach Poise as the event handler…
    // 4) And register Songbird on the same builder
    let mut client = ClientBuilder::new(token, intents)
        .framework(framework)
        .register_songbird_from_config(songbird_config) // ← this injects the Songbird voice manager
        .await
        .context("Failed to create the Discord client")?;

    tracing::info!("Starting Discord gateway");
    client
        .start()
        .await
        .context("Discord gateway stopped with an error")?;

    Ok(())
}
