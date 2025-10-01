mod commands;
mod definitions;
mod json_handling;

////////////////////////////////////////////////////////////////////////////////
/// Imports

use std::fs;
use std::path::PathBuf;
use std::collections::HashMap;
use poise::serenity_prelude::{ClientBuilder, GatewayIntents};
use songbird::SerenityInit; // brings in `.register_songbird()`
use tokio::sync::RwLock;
use dotenv::dotenv;

use crate::definitions::{Data, Error, TrackInfo};

////////////////////////////////////////////////////////////////////////////////
// Functions

async fn load_media() -> Result<RwLock<HashMap<String, TrackInfo>>, Error> {
    let mut library = HashMap::new();
    let paths = fs::read_dir("media/metadata").unwrap();

    for path in paths {
        let path = path.unwrap().path();
        if path.extension().unwrap() == "json" {
            let read_track_info = load_track(path).await;
            library.insert(
                read_track_info.id.clone(),
                read_track_info
            );
        }
    }

    Ok(RwLock::new(library))
}

async fn load_track(metadata_path: PathBuf) -> TrackInfo {
    let data = fs::read_to_string(metadata_path).expect("Metadata file should exist and doesn't");
    let track: TrackInfo = serde_json::from_str(&data).expect("Failed to deserialise JSON");
    track
}   

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
    std::env::set_current_dir(env!("CARGO_MANIFEST_DIR")).expect("Encountered an error setting the CWD to top-level");

    dotenv().ok();
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
        commands::paginate(),
        commands::pause()
    ];

    let poise_options = poise::FrameworkOptions {
        commands: poise_commands,
        prefix_options: poise::PrefixFrameworkOptions {
            prefix: Some(">".into()),
            // edit_tracker: Some(Arc::new(poise::EditTracker::for_timespan(
            //     Duration::from_secs(3600),
            // ))),
            // additional_prefixes: vec![
            //     poise::Prefix::Literal("hey bot,"),
            //     poise::Prefix::Literal("hey bot"),
            // ],
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
        // Every command invocation must pass this check to continue execution
        // command_check: Some(|ctx| {
        //     Box::pin(async move {
        //         if ctx.author().id == 123456789 {
        //             return Ok(false);
        //         }
        //         Ok(true)
        //     })
        // }),
        // Enforce command checks even for owners (enforced by default)
        // Set to true to bypass checks, which is useful for testing
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

    let library = load_media().await?;

    // 1) Build your Poise framework
    let framework = poise::Framework::builder()
        .options(poise_options)
        .setup(|_ctx, _ready, _framework| {
            Box::pin(async move {
                // poise::builtins::register_globally(ctx, &framework.options().commands).await?;
                Ok(
                    Data { 
                        library,
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
