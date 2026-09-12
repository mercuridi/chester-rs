use std::sync::Arc;

use anyhow::{Context, Result, bail};
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
        Chronicle, ChronicleDependencies, Embedder, GpuRuntime, Indexer, IndexerDb, Llm,
        RecorderManager, Retriever, TranscriptionService, notify_recording_user, report_cuda_oom,
        scan_incomplete_manifests,
    },
    config::{AppPaths, Config},
    database,
    discord::context::{Data, Error},
    jester::{
        library::{SyncConfig, sync_audio_library},
        track::{DownloadConfig, Downloader},
    },
    shutdown,
};

pub async fn run(paths: AppPaths, shutdown_timeout: std::time::Duration) -> Result<()> {
    tracing::info!(runtime_root = %paths.runtime_root.display(), config_path = %paths.config_path.display(), "Starting Chester");

    let (config, token) = load_startup(paths)?;
    let (pool, chronicle) = initialize_services(&config).await?;
    let downloader = synchronize_audio_library(&config, &pool).await?;
    run_discord_client(config, token, pool, chronicle, downloader, shutdown_timeout).await
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
        .map_err(anyhow::Error::from)
        .inspect_err(|error| {
            report_cuda_oom(error, "embedding", "device_initialization");
        })
        .context("Failed to select a CUDA or CPU device for Chronicle embeddings")?;
    let embedder = Embedder::load(device)
        .inspect_err(|error| {
            report_cuda_oom(error, "embedding", "load");
        })
        .context("Failed to load the Chronicle embedding model")?;
    let indexer = Indexer::new(
        config.chronicle.indexing.corpus_dir.clone(),
        chronicle_db,
        embedder,
        config.chronicle.indexing.max_chunk_tokens,
        config.chronicle.indexing.chunk_overlap_tokens,
    )
    .with_excluded_note_ids(config.chronicle.indexing.excluded_note_ids.clone());

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
    let llm = Arc::new(Llm::new(&config.chronicle.llm, runtime.clone()));
    let retriever = Arc::new(Retriever::new(chronicle_db.clone()));
    tracing::info!("Chronicle services initialized");

    Ok(Chronicle::new(
        config.chronicle.retrieval,
        config.chronicle.synthesis,
        config.chronicle.llm.generation.clone(),
        ChronicleDependencies {
            retriever,
            structured_store: Arc::new(chronicle_db),
            llm,
            runtime: runtime.clone(),
            transcription: TranscriptionService::new(runtime),
        },
    ))
}

fn build_commands() -> Vec<poise::Command<Data, Error>> {
    vec![
        crate::discord::commands::help(),
        crate::discord::commands::register(),
        crate::discord::commands::join(),
        crate::discord::commands::play(),
        crate::discord::commands::mix(),
        crate::discord::commands::queue(),
        crate::discord::commands::skip(),
        crate::discord::commands::history(),
        crate::discord::commands::leave(),
        crate::discord::commands::loop_track(),
        crate::discord::commands::pause(),
        crate::discord::commands::now_playing(),
        crate::discord::commands::download(),
        crate::discord::commands::reset_taxonomy(),
        crate::discord::commands::set_taxonomy(),
        crate::discord::commands::add_texture(),
        crate::discord::commands::add_environment(),
        crate::discord::commands::add_label(),
        crate::discord::commands::set_metadata(),
        crate::discord::commands::fix(),
        crate::discord::commands::library(),
        crate::discord::commands::recording(),
        crate::discord::commands::transcript(),
        crate::discord::commands::chronicle(),
    ]
}

async fn on_error(error: poise::FrameworkError<'_, Data, Error>) {
    match &error {
        poise::FrameworkError::Setup {
            error: setup_err, ..
        } => {
            tracing::error!(error = ?setup_err, "Failed to start bot");
        }
        poise::FrameworkError::Command {
            ctx,
            error: cmd_err,
            ..
        } => {
            tracing::warn!(
                command = %ctx.command().name,
                error_chain = %format!("{cmd_err:#}"),
                "Command failed"
            );
        }
        _ => {}
    }

    if let Err(error) = poise::builtins::on_error(error).await {
        tracing::error!("Error while handling error: {}", error);
    }
}

fn handle_event<'a>(
    ctx: &'a SerenityContext,
    event: &'a FullEvent,
    _framework: poise::FrameworkContext<'a, Data, Error>,
    data: &'a Data,
) -> poise::BoxFuture<'a, Result<(), Error>> {
    Box::pin(async move {
        if data.shutdown.is_requested() {
            return Ok(());
        }
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
            if user_id == initiator {
                return Ok(());
            }
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
    data: Data,
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
        .setup(|_ctx, _ready, _framework| Box::pin(async move { Ok(data) }))
        .build()
}

fn load_startup(paths: AppPaths) -> Result<(Config, String)> {
    let config_path = paths.config_path.clone();
    let config = Config::load(paths).with_context(|| {
        format!(
            "Failed to load configuration from {}",
            config_path.display()
        )
    })?;
    tracing::debug!(
        corpus_dir = %config.chronicle.indexing.corpus_dir.display(),
        retrieval_limit = config.chronicle.retrieval.limit,
        max_chunk_tokens = config.chronicle.indexing.max_chunk_tokens,
        max_reply_length = config.chronicle.llm.generation.max_reply_length,
        "Loaded configuration"
    );
    scan_incomplete_manifests(&config.paths.recordings_dir)
        .context("Failed to scan recording manifests")?;
    let token = std::env::var("DISCORD_TOKEN").context("DISCORD_TOKEN is not set")?;
    Ok((config, token))
}

async fn initialize_services(config: &Config) -> Result<(SqlitePool, Arc<Chronicle>)> {
    let pool = database::open_sqlite_pool(&config.database.jester, "Jester")
        .await
        .context("Failed to open the Jester database")?;
    crate::jester::db::initialise(&pool)
        .await
        .context("Failed to initialize the Jester database schema")?;
    let chronicle = Arc::new(
        build_chronicle(config)
            .await
            .context("Failed to initialize Chronicle")?,
    );
    Ok((pool, chronicle))
}

async fn synchronize_audio_library(config: &Config, pool: &SqlitePool) -> Result<Arc<Downloader>> {
    let downloader = Downloader::new(DownloadConfig::from(&config.paths));
    let sync_stats = sync_audio_library(
        pool,
        SyncConfig {
            downloader: downloader.clone(),
        },
    )
    .await
    .context("Failed to synchronize the audio library")?;
    info!(
        total_tracks = sync_stats.total_tracks,
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
    Ok(downloader)
}

async fn run_discord_client(
    config: Config,
    token: String,
    pool: SqlitePool,
    chronicle: Arc<Chronicle>,
    downloader: Arc<Downloader>,
    shutdown_timeout: std::time::Duration,
) -> Result<()> {
    let poise_commands = build_commands();
    tracing::info!(
        command_count = poise_commands.len(),
        "Registering bot commands"
    );
    let songbird_config =
        SongbirdConfig::default().decode_mode(DecodeMode::Decode(DecodeConfig::default()));
    let songbird = songbird::Songbird::serenity_from_config(songbird_config);
    let recorder = RecorderManager::new(config.paths.recordings_dir.clone());
    let player = Arc::new(crate::jester::player::PlayerService::new(
        config.paths.audio_dir.clone(),
    ));
    let coordinator = Arc::new(shutdown::ShutdownCoordinator::new(
        recorder.clone(),
        player.clone(),
        chronicle.clone(),
        pool.clone(),
        songbird.clone(),
        chronicle.transcription_service(),
        shutdown_timeout,
    ));
    let data = Data::new(
        pool,
        config,
        chronicle,
        recorder,
        player,
        downloader,
        coordinator.state.clone(),
    );
    let framework = build_framework(data, poise_commands);
    let intents = GatewayIntents::non_privileged() | GatewayIntents::MESSAGE_CONTENT;
    let mut client = ClientBuilder::new(token, intents)
        .framework(framework)
        .register_songbird_with(songbird)
        .await
        .context("Failed to create the Discord client")?;

    tracing::info!("Starting Discord gateway");
    let signal = shutdown_signal();
    tokio::pin!(signal);
    tokio::select! {
        result = client.start() => {
            let gateway_result = result.context("Discord gateway stopped with an error");
            let drain_result = coordinator.drain().await;
            gateway_result.and(drain_result)?;
        }
        signal_result = &mut signal => {
            signal_result?;
            tracing::info!("Shutdown signal received");
            coordinator.drain().await?;
        }
    }
    Ok(())
}

async fn shutdown_signal() -> Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate =
            signal(SignalKind::terminate()).context("Failed to install SIGTERM handler")?;
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    Ok(())
}
