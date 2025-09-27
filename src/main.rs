use std::{
    collections::HashMap,
    sync::Arc,
    path::PathBuf
};

use ::serenity::prelude::TypeMapKey;
use tokio::sync::Mutex;

use songbird::{
    driver::Bitrate, input::{cached::{Compressed, Memory}, File, Input}, Call, SerenityInit
};

use poise::serenity_prelude as serenity;

use serenity::{
    ChannelId,
    GuildId
};

use yt_dlp::Youtube;
use yt_dlp::fetcher::deps::Libraries;

// Read the bot token from a .env
use dotenv::dotenv;

struct TrackInfo {
        upload_date: String,
        duration_string: String,
        yt_title: String,
        yt_channel: String,
        track_title: String,
        track_artist: String
}

struct Library {
    track_ids: Vec<String>,
    track_infos: HashMap<String, TrackInfo>
}


struct Data {} // User data, which is stored and accessible in all command invocations
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

struct SoundStore;

impl TypeMapKey for SoundStore {
    type Value = Arc<Mutex<HashMap<String, CachedSound>>>;
}
struct Handler;

#[serenity::async_trait]
impl serenity::all::EventHandler for Handler {
    async fn ready(&self, _: serenity::Context, ready: serenity::all::Ready) {
        println!("{} is connected!", ready.user.name);
    }
}

async fn get_call_info(ctx: Context<'_>) -> Result<(GuildId, ChannelId), Error> {

    let guild_id = ctx.guild_id().unwrap();

    let cache = ctx.serenity_context().cache.clone();

    let voice_state = cache
        .guild(guild_id)
        .and_then(|g| g.voice_states.get(&ctx.author().id).cloned());
    let voice_channel_id = match voice_state.and_then(|vs| vs.channel_id) {
        Some(c) => c,
        None => {
            return Err("The user is not in a voice channel.".into());
        }
    };

    Ok((guild_id, voice_channel_id))
}


async fn join_vc(ctx: Context<'_>) -> Result<Arc<Mutex<Call>>, anyhow::Error> {

    let serenity_ctx = ctx.serenity_context();

    let manager = songbird::get(serenity_ctx)
        .await
        .expect("Songbird was not initialized")
        .clone();

    let call_info_result = get_call_info(ctx).await;

    let (guild_id, voice_channel_id) = match call_info_result {
        Ok(info) => info,
        Err(error) => {
            return Err(anyhow::anyhow!(error));
        }
    };

    let call = manager
        .join(guild_id, voice_channel_id)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to join voice channel: {:?}", e))?;

    ctx.say("Joined!").await?;
    Ok(call)
}

#[poise::command(slash_command)]
async fn download(
    ctx: Context<'_>,
    #[description = "YouTube link to download from"] yt_link: String
) -> Result<(), Error> {
    let libraries_dir = PathBuf::from("libs");
    let output_dir = PathBuf::from("output");
    
    let youtube = libraries_dir.join("yt-dlp");
    let ffmpeg = libraries_dir.join("ffmpeg");
    
    let libraries = Libraries::new(youtube, ffmpeg);
    let fetcher = Youtube::new(libraries, output_dir)?;
    
    let video = fetcher.fetch_video_infos(yt_link).await?;
    let video_id = &video.id;
    println!("Video title: {}", video.title);

    let audio_format = video.best_audio_format().unwrap();
    let audio_path = fetcher.download_format(&audio_format, format!("library/audio/{video_id}.mp3")).await?;
    Ok(())
}

#[poise::command(slash_command)]
async fn play(
    ctx: Context<'_>,
    #[description = "Selected track ID"] track_id: String
) -> Result<(), Error> {

    join_vc(ctx).await?;

    let call_info_result = get_call_info(ctx).await;

    let (guild_id, _voice_channel_id) = match call_info_result {
        Ok(info) => info,
        Err(error) => {
            return Err(error);
        }
    };

    let serenity_ctx = ctx.serenity_context();

    let manager = songbird::get(serenity_ctx)
        .await
        .expect("Songbird was not initialized")
        .clone();

    let song_src = Compressed::new(
        File::new(format!("library/audio/{track_id}.mp3")).into(),
        Bitrate::BitsPerSecond(128_000),
    )
        .await
        .expect("An error occurred constructing the track source");
    let _ = song_src.raw.spawn_loader();

    if let Some(handler_lock) = manager.get(guild_id) {
        let mut handler = handler_lock.lock().await;
        let _sound = handler.play_input(song_src.into());
    }

    Ok(())
}

#[tokio::main]
async fn main() {
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
                play(),
                download(),
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
        .expect("Err creating client");

    tokio::spawn(async move {
        let _ = client
            .start()
            .await
            .map_err(|why| println!("Client ended: {:?}", why));
    });

    let _signal_err = tokio::signal::ctrl_c().await;
    println!("Received Ctrl-C, shutting down.");

}