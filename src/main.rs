mod constants;
mod utils;
mod discord;
mod jester;
mod chronicle;

use std::path::PathBuf;

////////////////////////////////////////////////////////////////////////////////
/// Imports

use poise::serenity_prelude::{ClientBuilder, GatewayIntents};
use serenity::client::FullEvent;
use songbird::{Config as SongbirdConfig, SerenityInit, driver::{DecodeConfig, DecodeMode}}; use sqlx::SqlitePool;
use dotenv::dotenv;
use tracing::info;

use crate::{chronicle::{config::Config, indexer::{db::repository::IndexerDb, embedder::Embedder, indexer::Indexer}, llm::Llm, recording::recorder::notify_recording_user, service::Chronicle}, discord::context::{Data, Error}, jester::library::sync::sync_audio_library};
use tracing_subscriber::EnvFilter;

////////////////////////////////////////////////////////////////////////////////
// Functions

async fn on_error(error: poise::FrameworkError<'_, Data, Error>) {
    // 1) Inspect & log any command errors without moving out of `error`
    match &error {
        // Panic on setup failures
        poise::FrameworkError::Setup { error: setup_err, .. } => {
            panic!("Failed to start bot: {:?}", setup_err);
        }
        // Log command errors
        poise::FrameworkError::Command { ctx, error: cmd_err, .. } => {
            tracing::debug!("Error in command `{}`: {:?}", ctx.command().name, cmd_err);
        }
        // You can match other variants here if you like...
        _ => {}
    }

    // 2) Forward the _owned_ `error` to Poise's default handler so it replies in Discord
    if let Err(e) = poise::builtins::on_error(error).await {
        tracing::error!("Error while handling error: {}", e);
    }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env()
        .add_directive("chester_rs=debug".parse().unwrap())
        .add_directive("warn".parse().unwrap()))
        .init();

    dotenv().ok();
    // Initialize the SQLite connection pool
    tracing::debug!("Initialising player database connection");
    let database_url = "sqlite://database/jester/jester.sqlite3";
    let pool = SqlitePool::connect(database_url).await?;
    tracing::debug!("player database connection successful");

    std::env::set_current_dir(env!("CARGO_MANIFEST_DIR")).expect("Encountered an error setting the CWD to top-level");

    let config = Config::load(".chronicle/config.toml")?;

    tracing::info!(
        index_db = %config.chronicle.index_db,
        "Opening Chronicle index database"
    );

    let index_db = IndexerDb::open(&config.chronicle.index_db).await?;

    let embedder = Embedder::load(candle_core::Device::cuda_if_available(0)?)?;

    let indexer = Indexer::new(
        PathBuf::from(&config.chronicle.corpus_dir),
        index_db,
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

    let (index_db, embedder) = indexer.into_parts();

    let llm = Llm::new(
        &config.chronicle.llm_url,
        &config.chronicle.llm_model,
        config.chronicle.llm_max_tokens,
        config.chronicle.llm_temperature,
    );

    let chronicle = Chronicle::new(
        index_db,
        embedder,
        llm,
        config.chronicle.retrieval_limit,
    );

    let sync_stats = sync_audio_library(&pool).await?;

    info!(
        downloaded = sync_stats.downloaded,
        failed = sync_stats.failed,
        skipped = sync_stats.skipped,
        "Library sync complete"
    );

    let token = std::env::var("DISCORD_TOKEN").expect("missing DISCORD_TOKEN in .env");

    let poise_commands = vec![
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
        discord::commands::chronicle::ask(),
    ];

    let poise_options = poise::FrameworkOptions {
        commands: poise_commands,
        prefix_options: poise::PrefixFrameworkOptions {
            prefix: Some(">".into()),
            ..Default::default()
        },
        // The global error handler for all error cases that may occur
        on_error: |error| Box::pin(on_error(error)),
        // This code is run before every command
        pre_command: |ctx| {
            Box::pin(async move {
                tracing::debug!("Executing command {}...", ctx.command().qualified_name);
            })
        },
        // This code is run after a command if it was successful (returned Ok)
        post_command: |ctx| {
            Box::pin(async move {
                tracing::debug!("Successfully executed command {}", ctx.command().qualified_name);
            })
        },
        skip_checks_for_owners: true,
        event_handler: |ctx, event, _framework, data| {
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

                    // The initiator is deliberately excluded from notifications.
                    if user_id == initiator {
                        return Ok(());
                    }

                    // Only notify when the user enters the recording channel.
                    //
                    // This covers:
                    //   None -> recording channel
                    //   other VC -> recording channel
                    //
                    // It excludes:
                    //   recording channel -> same channel
                    //   recording channel -> other VC
                    //   recording channel -> None
                    if new.channel_id != Some(voice_channel_id) {
                        return Ok(());
                    }

                    if old.as_ref().and_then(|state| state.channel_id) == Some(voice_channel_id) {
                        return Ok(());
                    }

                    notify_recording_user(
                        &ctx.http,
                        notification_channel_id,
                        user_id,
                    )
                    .await?;

                }

                Ok(())
            })
        },
        ..Default::default()
    };

    // 1) Build your Poise framework
    let framework = poise::Framework::builder()
        .options(poise_options)
        .setup(|_ctx, _ready, _framework| {
            Box::pin(async move {
                // poise::builtins::register_globally(ctx, &framework.options().commands).await?;
                Ok(Data::new(pool, config, chronicle))
            })
        })
        .build();

    let intents = GatewayIntents::non_privileged() | GatewayIntents::MESSAGE_CONTENT;

    // 2) Build the Songbird config too (required for decoding voice data)
    let songbird_config = SongbirdConfig::default()
        .decode_mode(DecodeMode::Decode(DecodeConfig::default()));

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
