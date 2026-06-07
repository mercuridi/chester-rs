mod constants;
mod definitions;
mod db;
mod discord;
mod player;
mod utils;

////////////////////////////////////////////////////////////////////////////////
/// Imports

use poise::serenity_prelude::{ClientBuilder, GatewayIntents};
use songbird::SerenityInit; use sqlx::SqlitePool;
use dotenv::dotenv;

use crate::definitions::{Data, Error};

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
    let database_url = "sqlite://database/metadata.sqlite3";
    let pool = SqlitePool::connect(database_url).await?;

    std::env::set_current_dir(env!("CARGO_MANIFEST_DIR")).expect("Encountered an error setting the CWD to top-level");

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
        discord::commands::browse::library(),
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
                tracing::debug!("Executed command {}!", ctx.command().qualified_name);
            })
        },
        skip_checks_for_owners: true,
        event_handler: |_ctx, event, _framework, _data| {
            Box::pin(async move {
                tracing::debug!(
                    "Got an event in event handler: {:?}",
                    event.snake_case_name()
                );
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
                Ok(Data::new(pool))
            })
        })
        .build();

    let intents = GatewayIntents::non_privileged() | GatewayIntents::MESSAGE_CONTENT;

    // 2) Create the Serenity client, attach Poise as the event handler…
    // 3) And register Songbird on the same builder
    let mut client = ClientBuilder::new(token, intents)
        .framework(framework)
        .register_songbird() // ← this injects the Songbird voice manager
        .await?;

    // 4) Start the bot
    client.start().await?;

    Ok(())
}
