////////////////////////////////////////////////////////////////////////////////
// Imports
use std::sync::Arc;
use std::fs::write;
use std::path::PathBuf;

use poise::serenity_prelude::{ChannelId, Guild, AutocompleteChoice};
use songbird::input::File as SongbirdFile;
use songbird::input::cached::Compressed;
use songbird::driver::Bitrate;
use songbird::tracks::LoopState;
use songbird::Call;
use yt_dlp::Youtube;
use yt_dlp::fetcher::deps::Libraries;
use tokio::sync::Mutex;
use crate::definitions::{Context, Error, TrackInfo, Data};

////////////////////////////////////////////////////////////////////////////////
// Helper functions

async fn write_track_metadata (
    track_info_to_write: TrackInfo
) -> Result<(), Error> {
    let video_id = track_info_to_write.id.clone();
    write(format!("media/metadata/{video_id}.json"), serde_json::to_string_pretty(&track_info_to_write)?).expect("Failed to write metadata file");
    Ok(())
}

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


async fn autocomplete_tracks(
    ctx: Context<'_>,
    partial: &str,
) -> impl Iterator<Item = AutocompleteChoice> {
    let data: &Data = ctx.data();
    let library = data.library.read().await; // tokio::sync::RwLock

    let needle = partial.to_lowercase();
    let mut choices: Vec<AutocompleteChoice> = Vec::with_capacity(25);

    for info in library.values() {
        // Build a display name
        let display = format!("{} | {} | {} | {:?}",
            info.track_title.clone(),
            info.track_artist.clone(),
            info.track_origin.clone(),
            info.tags.clone().join(", "),
        );

        // Build the search string
        let search = format!("{} {} {} {:?}",
            info.track_title,
            info.track_artist,
            info.track_origin,
            info.tags.join(", "),
        );
        if partial.is_empty() || search.to_lowercase().contains(&needle) {
            choices.push(
                AutocompleteChoice::new(
                    display,
                    info.id.clone(), // use the unique id as the value
                )
            );
            if choices.len() >= 25 {
                break;
            }
        }
    }

    choices.into_iter()
}

async fn autocomplete_attributes(
    _ctx: Context<'_>,
    _partial: &str,
) -> impl Iterator<Item = String> {
    ["title", "artist", "origin"].iter().map(|s| s.to_string())
}

////////////////////////////////////////////////////////////////////////////////
// Command definitions

// Commands: Bot management
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

// Commands: Library management

/// Reset a track's user-set metadata tags
#[poise::command(slash_command)]
pub async fn reset_tags(
    ctx: Context<'_>,
    #[description = "The track to reset the tags of"]
    #[autocomplete = "autocomplete_tracks"]
    track_id: String
) -> Result<(), Error> {
    let data: &Data = ctx.data();
    let mut library = data.library.write().await; // tokio::sync::RwLock
    // Look up the TrackInfo by key and clear its tags
    if let Some(track_info) = library.get_mut(&track_id) {
        track_info.tags.clear();
        ctx.say(format!("Reset tags for track `{}`", track_info.track_title)).await?;
    } else {
        ctx.say(format!("No track found with id `{}`", track_id)).await?;
    }

    Ok(())
}

/// Add a new arbitrary tag to a track
#[poise::command(slash_command)]
pub async fn add_tag(
    ctx: Context<'_>,
    #[description = "The track to add a tag to"]
    #[autocomplete = "autocomplete_tracks"]
    track_id: String,
    #[description = "The tag to add"] tag: String
) -> Result<(), Error> {
    let data: &Data = ctx.data();
    let mut library = data.library.write().await; // tokio::sync::RwLock
    // Look up the TrackInfo by key and clear its tags
    if let Some(track_info) = library.get_mut(&track_id) {
        track_info.tags.push(tag.clone());
        write_track_metadata(track_info.clone()).await?;
        ctx.say(format!("Tag `{}` added to track `{}`", tag, track_info.track_title)).await?;
    } else {
        ctx.say(format!("No track found with id `{}`", track_id)).await?;
    }

    Ok(())
}

/// Set a track's title, artist, or origin
#[poise::command(slash_command)]
pub async fn set_metadata(
    ctx: Context<'_>,
    #[description = "The track to adjust"]
    #[autocomplete = "autocomplete_tracks"]
    track_id: String,
    #[description = "The attribute to adjust"] 
    #[autocomplete = "autocomplete_attributes"]
    attribute: String,
    #[description = "The new value to give the attribute"] 
    new_value: String
) -> Result<(), Error> {
    let data: &Data = ctx.data();
    let mut library = data.library.write().await; // tokio::sync::RwLock
    // Look up the TrackInfo by key and clear its tags
    if let Some(track_info) = library.get_mut(&track_id) {
        let pocket = new_value.clone();
        match attribute.as_str() {
            "title" => track_info.track_title = new_value,
            "artist" => track_info.track_artist = new_value,
            "origin" => track_info.track_origin = new_value,
            other => return Err(format!("Unknown attribute `{}`. Please pick an autocomplete option.", other).into())
        }
        write_track_metadata(track_info.clone()).await?;
        ctx.say(format!("Set attribute `{}` as `{}` for track `{}`", attribute, pocket, track_info.track_title)).await?;
    } else {
        ctx.say(format!("No track found with id `{}`", track_id)).await?;
    }

    Ok(())
}

/// Download a track from a YouTube link
/// Leaving options blank will copy them from the YouTube video
#[poise::command(slash_command)]
pub async fn download(
    ctx: Context<'_>,
    #[description = "YouTube link to download from"] yt_link: String,
    #[description = "The actual title of the track"] track_title: Option<String>,
    #[description = "The actual artist of the track"] track_artist: Option<String>,
    #[description = "The origin of the track (eg. game/movie title)"] track_origin: Option<String>
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
    println!("Audio downloaded @ {}", audio_path.display());

    let track_title = match track_title {
        Some(title) => title,
        None => video.title.clone()
    };

    let track_artist = match track_artist {
        Some(artist) => artist,
        None => video.channel.clone()
    };

    let track_origin = match track_origin {
        Some(origin) => origin,
        None => "No origin provided".into()
    };

    let new_track = TrackInfo {
        id: video_id.clone(),
        upload_date: video.upload_date,
        yt_title: video.title.clone(),
        yt_channel: video.channel,
        track_title,
        track_artist,
        track_origin,
        tags: Vec::new(),
    };

    write_track_metadata(new_track).await?;

    let video_title = video.title;

    ctx.say(format!("File downloaded: {video_title}")).await?;
    println!("Download finished");
    Ok(())
}

// Commands: Music control

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
    #[description = "Track to play now"]
    #[autocomplete = "autocomplete_tracks"]
    track_id: String
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

    if let Some(handler_lock) = manager.get(guild.id.clone()) {
        let mut handler = handler_lock.lock().await;
        let track_handle = handler.play_only_input(song_src.into());
        let data: &Data = ctx.data();
        let mut handles = data.track_handles.write().await; // tokio::sync::RwLock
        handles.insert(
            guild.id,
            track_handle
        );
    }

    ctx.say("Playing track now").await?;

    Ok(())
}

/// Loop or un‐loop the currently playing track.
#[poise::command(slash_command, prefix_command)]
pub async fn loop_track(
    ctx: Context<'_>,
) -> Result<(), Error> {
    // Make sure we're in a guild
    let guild_id = if let Some(g) = ctx.guild_id() {
        g
    } else {
        return Err(format!("Looping only works in a server").into())
    };

    // See if there's a current track
    let data: &Data = ctx.data();
    let handles = data.track_handles.read().await; // tokio::sync::RwLock
    if let Some(track_handle) = handles.get(&guild_id) {
        let handle_info = track_handle.clone().get_info().await?;
        let loops = handle_info.loops;
        let new_state: bool;
        match loops {
            LoopState::Infinite => {
                let _ = track_handle.disable_loop()?;
                new_state = false;
            },
            LoopState::Finite(_) => {
                let _ = track_handle.enable_loop()?;
                new_state = true;
            }
        }
        ctx.say(format!("Looping {}", if new_state { "enabled" } else { "disabled" })).await?;
    } else {
        ctx.say("No track is currently playing.").await?;
    };
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
