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
        File as SongbirdFile,
    },
    Call,
    SerenityInit
};

use poise::serenity_prelude as serenity;

use serenity::{
    ChannelId,
    GuildId
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


#[derive(Serialize, Deserialize)]
struct TrackInfo {
        upload_date: i64,
        yt_title: String,
        yt_channel: String,
        track_title: String,
        track_artist: String
}

struct MediaLibrary {
    track_ids: Vec<String>,
    track_infos: HashMap<String, TrackInfo>
}

struct Data {} // User data, which is stored and accessible in all command invocations
type Error = Box<dyn std::error::Error + Send + Sync>;
type Context<'a> = poise::Context<'a, Data, Error>;
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
    #[description = "YouTube link to download from"] yt_link: String,
    #[description = "The actual title of the track"] track_title: String,
    #[description = "The actual artist of the track"] track_artist: String
) -> Result<(), Error> {

    ctx.defer().await?;

    let libraries_dir = PathBuf::from("libs");
    let output_dir = PathBuf::from("media");

    let youtube = libraries_dir.join("yt-dlp");
    let ffmpeg = libraries_dir.join("ffmpeg");

    let libraries = Libraries::new(youtube, ffmpeg);
    let fetcher = Youtube::new(libraries, &output_dir)?;
    
    let video = fetcher.fetch_video_infos(yt_link).await?;
    let video_id = &video.id;
    println!("Video title: {}", video.title);
    
    let audio_format = video.best_audio_format().unwrap();
    let audio_path = fetcher.download_format(&audio_format, format!("audio/{video_id}.mp3")).await?;
    println!("Audio downloaded {}", audio_path.display());

    let new_track = TrackInfo {
        upload_date: video.upload_date,
        yt_title: video.title,
        yt_channel: video.channel,
        track_title,
        track_artist,
    };

    let j = serde_json::to_string_pretty(&new_track)?;
    write(format!("media/metadata/{video_id}.json"), j).expect("Failed to write metadata file");

    ctx.say("File downloaded").await?;
    println!("Download finished");
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
        SongbirdFile::new(format!("media/audio/{track_id}.mp3")).into(),
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

#[poise::command(slash_command)]
pub async fn register(ctx: Context<'_>) -> Result<(), Error> {
    poise::builtins::register_application_commands_buttons(ctx).await?;
    Ok(())
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