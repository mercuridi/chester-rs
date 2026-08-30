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
        indexer::{db::repository::IndexerDb, embedder::Embedder, service::Indexer},
        llm::Llm,
        recording::recorder::{notify_recording_user, scan_incomplete_manifests},
        runtime::GpuRuntime,
        service::Chronicle,
    },
    discord::context::{Data, Error},
    jester::library::sync::sync_audio_library,
};
use anyhow::{Context, Result, bail};
use tracing_subscriber::EnvFilter;

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
    );

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
        config.chronicle.llm_max_reply_length,
    ))
}

fn build_commands() -> Vec<poise::Command<Data, Error>> {
    vec![
        discord::commands::admin::help(),
        discord::commands::admin::register(),
        discord::commands::controls::join(),
        discord::commands::controls::play(),
        discord::commands::controls::leave(),
        discord::commands::controls::loop_track(),
        discord::commands::controls::pause(),
        discord::commands::controls::now_playing(),
        discord::commands::management::download(),
        discord::commands::management::reset_tags(),
        discord::commands::management::add_tag(),
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
    // Keep normal operation useful without being noisy. Set RUST_LOG to override,
    // for example: `RUST_LOG=chester_rs=debug cargo run`.
    let (env_filter, invalid_filter) = match EnvFilter::try_from_default_env() {
        Ok(filter) => (filter, None),
        Err(error) => (EnvFilter::new("chester_rs=info,warn"), Some(error)),
    };

    #[allow(clippy::print_stderr)]
    if let Err(error) = tracing_subscriber::fmt()
        .with_env_filter(env_filter)
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

async fn run() -> Result<()> {
    let project_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    from_path(project_root.join(".env")).ok();

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
