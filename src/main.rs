////////////////////////////////////////////////////////////////////////////////
/// Imports

use std::{
    collections::HashMap,
    fs::{
        self,
        write
    }, 
    path::{
        PathBuf,
        Path
    },
    sync::Arc,
};

use serde::{
    Deserialize,
    Serialize
};

use tokio::sync::Mutex;

use songbird::{
    driver::Bitrate,
    input::{
        cached::Compressed,
        cached::Memory,
        File as SongbirdFile,
        Input
    },
    Call,
    SerenityInit
};

use poise::serenity_prelude as serenity;

use serenity::{
    ChannelId,
    GuildId,
    prelude::TypeMapKey
};

use yt_dlp::{
    fetcher::deps::{
        Libraries,
        LibraryInstaller
    },
    Youtube
};

// Read the bot token from a .env
use dotenv::dotenv;

////////////////////////////////////////////////////////////////////////////////
/// Type and struct definitions

type Error = Box<dyn std::error::Error + Send + Sync>;
type Context<'a> = poise::Context<'a, Data, Error>;
enum CachedSound {
    Compressed(Compressed),
    Uncompressed(Memory),
}

impl From<&CachedSound> for Input {
    fn from(obj: &CachedSound) -> Self {
        use CachedSound::*;
        match obj {
            Compressed(c) => c.new_handle().into(),
            Uncompressed(u) => u
                .new_handle()
                .try_into()
                .expect("Failed to create decoder for Memory source."),
        }
    }
}

struct Handler;

#[serenity::async_trait]
impl serenity::all::EventHandler for Handler {
    async fn ready(&self, _: serenity::Context, ready: serenity::all::Ready) {
        println!("{} is connected!", ready.user.name);
    }
}

struct Data {} // User data, which is stored and accessible in all command invocations

struct SoundStore;

impl TypeMapKey for SoundStore {
    type Value = Arc<Mutex<HashMap<String, CachedSound>>>;
}

////////////////////////////////////////////////////////////////////////////////
/// Function definitions

#[tokio::main]
async fn main() {
    std::env::set_current_dir(env!("CARGO_MANIFEST_DIR")).expect("Encountered an error setting the CWD to top-level");

    handle_libraries().await.unwrap();

    dotenv().ok();
    let token = std::env::var("DISCORD_TOKEN").expect("missing DISCORD_TOKEN in .env");
    let intents = 
            serenity::GatewayIntents::MESSAGE_CONTENT 
        |   serenity::GatewayIntents::GUILD_VOICE_STATES
        |   serenity::GatewayIntents::GUILDS
        |   serenity::GatewayIntents::GUILD_MESSAGES;

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: vec![
                register(),
            ],
            ..Default::default()
        })
        .setup(|ctx, _ready, framework| {
            Box::pin(async move {
                poise::builtins::register_globally(ctx, &framework.options().commands).await?;
                Ok(Data {})
            })
        })
        .build();

    let mut client = serenity::ClientBuilder::new(token, intents)
        .register_songbird()
        .framework(framework)
        .event_handler(Handler)
        .await
        .expect("Error creating client");

    tokio::spawn(async move {
        let _ = client
            .start()
            .await
            .map_err(|why| println!("Client ended: {:?}", why));
    });

    let _signal_err = tokio::signal::ctrl_c().await;
    println!("Received Ctrl-C, shutting down.");
}

async fn handle_libraries() -> Result<(), Error> {
    // make path if it doesn't exist
    if !Path::new("libs/").exists() {
        fs::create_dir("libs")?;
    }

    let destination = PathBuf::from("libs");
    let installer = LibraryInstaller::new(destination);

    // install ffmpeg if it isn't there
    if fs::metadata("libs/ffmpeg").is_err() {
        let _ffmpeg = installer.install_ffmpeg(None).await.unwrap();
    }

    // install ytdlp if it isn't there
    if fs::metadata("libs/yt-dlp").is_err() {
        let _youtube = installer.install_youtube(None).await.unwrap();
    }

    Ok(())
}

#[poise::command(slash_command)]
pub async fn register(ctx: Context<'_>) -> Result<(), Error> {
    poise::builtins::register_application_commands_buttons(ctx).await?;
    Ok(())
}