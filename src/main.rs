mod chronicle;
mod constants;
mod database;
mod discord;
mod jester;
mod utils;

use std::path::PathBuf;

////////////////////////////////////////////////////////////////////////////////
use dotenv::dotenv;
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
        recording::recorder::notify_recording_user,
        runtime::GpuRuntime,
        service::Chronicle,
    },
    discord::context::{Data, Error},
    jester::library::sync::sync_audio_library,
};
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

async fn build_chronicle(config: &Config) -> Result<Chronicle, Error> {
    tracing::info!(
        chronicle_db = %config.database.chronicle,
        "Opening Chronicle index database"
    );

    let chronicle_db = IndexerDb::open(&config.database.chronicle).await?;
    tracing::info!("Loading Chronicle embedding model");
    let embedder = Embedder::load(candle_core::Device::cuda_if_available(0)?)?;
    let indexer = Indexer::new(
        PathBuf::from(&config.chronicle.corpus_dir),
        chronicle_db,
        embedder,
        config.chronicle.max_chunk_length,
    );

    let indexing_stats = indexer.index().await?;
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
async fn main() -> Result<(), Error> {
    dotenv().ok();

    // Keep normal operation useful without being noisy. Set RUST_LOG to override,
    // for example: `RUST_LOG=chester_rs=debug cargo run`.
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("chester_rs=info,warn"));

    tracing_subscriber::fmt().with_env_filter(env_filter).init();

    tracing::info!("Starting Chester");
    std::env::set_current_dir(env!("CARGO_MANIFEST_DIR"))?;

    let config = Config::load(".chronicle/config.toml")?;
    tracing::debug!(
        corpus_dir = %config.chronicle.corpus_dir,
        retrieval_limit = config.chronicle.retrieval_limit,
        max_chunk_length = config.chronicle.max_chunk_length,
        "Loaded configuration"
    );

    let pool = database::pool::open_sqlite_pool(&config.database.jester, "Jester").await?;
    jester::db::schema::initialise(&pool).await?;
    let chronicle = build_chronicle(&config).await?;

    let sync_stats = sync_audio_library(&pool).await?;

    info!(
        downloaded = sync_stats.downloaded,
        failed = sync_stats.failed,
        skipped = sync_stats.skipped,
        "Library sync complete"
    );

    let token = std::env::var("DISCORD_TOKEN")?;

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
        .await?;

    // 4) Start the bot
    client.start().await?;

    Ok(())
}
