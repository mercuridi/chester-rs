mod commands;
mod definitions;

use poise::serenity_prelude::{ClientBuilder, GatewayIntents, ChannelId};
use songbird::SerenityInit; // ← brings in `.register_songbird()`

use crate::definitions::{Context, Data, Error};

// custom user data passed to all functions via Serenity context

async fn on_error(error: poise::FrameworkError<'_, Data, Error>) {
    // This is our custom error handler
    // They are many errors that can occur, so we only handle the ones we want to customize
    // and forward the rest to the default handler
    match error {
        poise::FrameworkError::Setup { error, .. } => panic!("Failed to start bot: {:?}", error),
        poise::FrameworkError::Command { error, ctx, .. } => {
            println!("Error in command `{}`: {:?}", ctx.command().name, error,);
        }
        error => {
            if let Err(e) = poise::builtins::on_error(error).await {
                println!("Error while handling error: {}", e)
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let token = std::env::var("DISCORD_TOKEN")?;
    let poise_options = poise::FrameworkOptions {
        commands: vec![commands::join(), commands::register()],
        prefix_options: poise::PrefixFrameworkOptions {
            prefix: Some("~".into()),
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

    // 1) Build your Poise framework
    let framework = poise::Framework::builder()
        .options(poise_options)
        .setup(|ctx, _ready, framework| {
            Box::pin(async move {
                poise::builtins::register_globally(ctx, &framework.options().commands).await?;
                Ok(Data {})
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