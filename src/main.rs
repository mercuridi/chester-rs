mod commands;
mod definitions;
mod json_handling;
mod autocomplete;
mod constants;
mod library;
mod cmd_library;

////////////////////////////////////////////////////////////////////////////////
/// Imports

use std::collections::HashMap;
use poise::serenity_prelude::{ClientBuilder, GatewayIntents};
use songbird::SerenityInit; use sqlx::SqlitePool;
use tokio::sync::RwLock;
use dotenv::dotenv;

use crate::definitions::{Data, Error};

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
            println!("Error in command `{}`: {:?}", ctx.command().name, cmd_err);
        }
        // You can match other variants here if you like...
        _ => {}
    }

    // 2) Forward the _owned_ `error` to Poise's default handler so it replies in Discord
    if let Err(e) = poise::builtins::on_error(error).await {
        eprintln!("Error while handling error: {}", e);
    }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    dotenv().ok();
    // Initialize the SQLite connection pool
    let database_url = "sqlite://database/metadata.sqlite3";
    let pool = SqlitePool::connect(database_url).await?;

    std::env::set_current_dir(env!("CARGO_MANIFEST_DIR")).expect("Encountered an error setting the CWD to top-level");

    let token = std::env::var("DISCORD_TOKEN").expect("missing DISCORD_TOKEN in .env");

    let poise_commands = vec![
        commands::help(),
        commands::register(),
        commands::join(),
        commands::play(),
        commands::leave(),
        commands::download(),
        commands::reset_tags(),
        commands::add_tag(),
        commands::set_metadata(),
        commands::loop_track(),
        commands::pause(),
        cmd_library::library(),
        cmd_library::library_title(),
        cmd_library::library_artist(),
        cmd_library::library_origin(),
        cmd_library::library_tags(),
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
                println!("Executing command {}...", ctx.command().qualified_name);
            })
        },
        // This code is run after a command if it was successful (returned Ok)
        post_command: |ctx| {
            Box::pin(async move {
                println!("Executed command {}!", ctx.command().qualified_name);
            })
        },
        skip_checks_for_owners: true,
        event_handler: |_ctx, event, _framework, _data| {
            Box::pin(async move {
                println!(
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
                Ok(
                    Data { 
                        db_pool: pool,
                        track_handles: RwLock::new(HashMap::new())
                    }
                )
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
