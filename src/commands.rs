////////////////////////////////////////////////////////////////////////////////
// Imports
use std::sync::Arc;
use std::fs::write;
use std::path::PathBuf;

use poise::serenity_prelude::{ChannelId, Guild};
use songbird::input::File as SongbirdFile;
use songbird::input::cached::Compressed;
use songbird::driver::Bitrate;
use songbird::Call;
use yt_dlp::Youtube;
use yt_dlp::fetcher::deps::Libraries;
use tokio::sync::Mutex;
use crate::definitions::{Context, Error, TrackInfo};

////////////////////////////////////////////////////////////////////////////////
// Helper functions

async fn get_vc_id(ctx: Context<'_>) -> Result<ChannelId, Error> {

    let guild_id = ctx.guild_id().unwrap();

    let voice_state = ctx.serenity_context()
        .cache
        .clone()
        .guild(guild_id)
        .and_then(|g| g.voice_states.get(&ctx.author().id).cloned());
    let voice_channel_id = match voice_state.and_then(|vs| vs.channel_id) {
        Some(c) => c,
        None => return Err("The user is not in a voice channel.".into())
    };

    Ok(voice_channel_id)
}

async fn join_vc(ctx: Context<'_>, guild: Guild, vc_id: ChannelId) -> Result<Arc<Mutex<Call>>, Error>{
    let manager = songbird::get(ctx.serenity_context())
        .await
        .expect("Error getting the Songbird client from the manager")
        .clone();

    let join_result = manager.join(guild.id, vc_id).await;
    Ok(join_result?)
}

////////////////////////////////////////////////////////////////////////////////
// Command definitions

/// Force-register commands - only invokes with ">"
#[poise::command(prefix_command)]
pub async fn register(ctx: Context<'_>) -> Result<(), Error> {
    poise::builtins::register_application_commands_buttons(ctx).await?;
    Ok(())
}

/// Shows help for commands
#[poise::command(prefix_command, slash_command)]
pub async fn help(
    ctx: Context<'_>,
    #[description = "Specific command to show help about"]
    #[autocomplete = "poise::builtins::autocomplete_command"]
    command: Option<String>,
) -> Result<(), Error> {
    poise::builtins::help(
        ctx,
        command.as_deref(),
        poise::builtins::HelpConfiguration {
            extra_text_at_bottom: "Chester is a Discord music bot that won't ask for your money.",
            ..Default::default()
        },
    )
    .await?;
    Ok(())
}

/// Joins your voice channel
#[poise::command(slash_command)]
pub async fn join(
    ctx: Context<'_>,
) -> Result<(), Error> {
    let guild = ctx.guild().expect("Must be in a guild to use voice").clone();
    let vc_id = get_vc_id(ctx).await?;

    join_vc(ctx, guild, vc_id).await?;

    ctx.say("Joined your voice channel! 🎶").await?;
    Ok(())
}

/// Plays a selected track from the library
#[poise::command(slash_command)]
pub async fn play(
    ctx: Context<'_>,
    #[description = "Selected track ID"] track_id: String
) -> Result<(), Error> {

    let guild = ctx.guild().expect("Must be in a guild to use voice").clone();
    let vc_id = get_vc_id(ctx).await?;

    let serenity_ctx = ctx.serenity_context();

    let manager = songbird::get(serenity_ctx)
        .await
        .expect("Songbird was not initialized")
        .clone();

    join_vc(ctx, guild.clone(), vc_id).await?;

    let song_src = Compressed::new(
        SongbirdFile::new(format!("media/audio/{track_id}.mp3")).into(),
        Bitrate::BitsPerSecond(128_000),
    )
        .await
        .expect("An error occurred constructing the track source");
    let _ = song_src.raw.spawn_loader();

    if let Some(handler_lock) = manager.get(guild.id) {
        let mut handler = handler_lock.lock().await;
        let _sound = handler.play_input(song_src.into());
    }

    ctx.say("Playing track now").await?;

    Ok(())
}

/// Leaves a joined voice channel
#[poise::command(slash_command)]
pub async fn leave(
    ctx: Context<'_>,
) -> Result<(), Error> {

    let guild = ctx.guild().expect("Must be in a guild to use voice").clone();
    let _vc_id = get_vc_id(ctx).await?;

    let serenity_ctx = ctx.serenity_context();

    let manager = songbird::get(serenity_ctx)
        .await
        .expect("Songbird was not initialized")
        .clone();

    manager.remove(guild.id).await?;

    ctx.say("Left the voice channel").await?;

    Ok(())
}

/// Download a track from a YouTube link
/// Leaving options blank will copy them from the YouTube video
#[poise::command(slash_command)]
pub async fn download(
    ctx: Context<'_>,
    #[description = "YouTube link to download from"] yt_link: String,
    #[description = "The actual title of the track"] track_title: Option<String>,
    #[description = "The actual artist of the track"] track_artist: Option<String>
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
    let video_title = video.title.clone();
    println!("Video title: {}", video.title);
    
    let audio_format = video.best_audio_format().unwrap();
    let audio_path = fetcher.download_format(&audio_format, format!("audio/{video_id}.mp3")).await?;
    println!("Audio downloaded {}", audio_path.display());

    let track_title = match track_title {
        Some(title) => title,
        None => video.title.clone()
    };

    let track_artist = match track_artist {
        Some(artist) => artist,
        None => video.channel.clone()
    };

    let new_track = TrackInfo {
        upload_date: video.upload_date,
        yt_title: video.title,
        yt_channel: video.channel,
        track_title,
        track_artist,
    };

    let j = serde_json::to_string_pretty(&new_track)?;
    write(format!("media/metadata/{video_id}.json"), j).expect("Failed to write metadata file");

    ctx.say(format!("File downloaded: {video_title}")).await?;
    println!("Download finished");
    Ok(())
}