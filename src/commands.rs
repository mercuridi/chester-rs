////////////////////////////////////////////////////////////////////////////////
// Imports
use std::sync::Arc;
use std::fs::write;
use std::process::Command;
use std::collections::HashSet;

use serde_json::Value;
use url::Url;
use poise::serenity_prelude::{ChannelId, Guild, AutocompleteChoice};
use songbird::input::File as SongbirdFile;
use songbird::input::cached::Compressed;
use songbird::driver::Bitrate;
use songbird::tracks::LoopState;
use songbird::Call;
use tokio::sync::Mutex;
use crate::definitions::{Context, Error, TrackInfo, Data};
use crate::json_handling::process_ytdlp_json;

////////////////////////////////////////////////////////////////////////////////
// Helper functions

// MAX here is 25 (Discord limitation)
const AUTOCOMPLETE_MAX_CHOICES: usize = 25;

// MAX here is 100 (Discord limitation)
const AUTOCOMPLETE_MAX_LENGTH: usize = 100;
const ELLIPSIS: &str = "…";
const SEPARATOR: &str = " | ";
const ELLIPSIS_LEN: usize = ELLIPSIS.len();
const SEPARATOR_LEN: usize = SEPARATOR.len();

fn build_autocomplete_display(mut to_display: Vec<String>) -> String {
    // Build a display name
    let content_max_length = AUTOCOMPLETE_MAX_LENGTH - (SEPARATOR_LEN * to_display.len()) + 1;

    let mut lens: Vec<usize> = to_display
        .iter()
        .map(|n| n.len())
        .collect();
    let total_len: usize = lens.iter().sum();
    let mut excess = total_len.saturating_sub(content_max_length);

    // truncate each as needed
    while excess > 0 {
        // pick the index of the longest field
        let (max_idx, &max_len) = lens
            .iter()
            .enumerate()
            .max_by_key(|&(_, &l)| l)
            .unwrap();

        // decide how many bytes to chop
        let chop = excess.min(max_len);
        let mut new_len = max_len.saturating_sub(chop);

        // reserve room for ellipsis if we're actually cutting
        let needs_ellipsis = new_len < max_len;
        if needs_ellipsis && new_len > ELLIPSIS_LEN {
            new_len = new_len.saturating_sub(ELLIPSIS_LEN);
        }

        // get the mutable String reference
        let s: &mut String = &mut to_display[max_idx];

        // back up to a valid UTF-8 boundary
        let mut adjust = new_len;
        while adjust > 0 && !s.is_char_boundary(adjust) {
            adjust -= 1;
        }
        s.truncate(adjust);

        // append ellipsis if we cut something
        if needs_ellipsis {
            s.push_str(ELLIPSIS);
            lens[max_idx] = adjust + ELLIPSIS_LEN;
        } else {
            lens[max_idx] = adjust;
        }

        excess = excess.saturating_sub(chop);
    }

    to_display.join(SEPARATOR)

}

fn lightweight_trim(mut choice: String) -> String {
    if choice.len() > 99 {
        choice.truncate(99);
        choice.push_str(ELLIPSIS);
    }
    choice
}

fn get_youtube_id(link: &str) -> Option<String> {
    // Try to parse the URL; bail out if it's invalid
    println!("Parsing YouTube link {}", link);
    let url = Url::parse(link).ok()?;
    let host = url.host_str()?;

    match host {
        // Short links: https://youtu.be/VIDEO_ID
        "youtu.be" => {
            // path_segments() -> segments between the slashes
            url.path_segments()
               .and_then(|mut segs| segs.next())
               .map(|id| id.to_string())
        }

        // Standard watch URLs, mobile, or www embeds
        "www.youtube.com" | "youtube.com" | "m.youtube.com" => {
            // 1) /watch?v=VIDEO_ID
            if let Some((_, v)) = url.query_pairs().find(|(k, _)| k == "v") {
                return Some(v.into_owned());
            }
            // 2) /embed/VIDEO_ID
            url.path_segments()
               .and_then(|mut segs| {
                   segs.find(|part| *part == "embed").and_then(|_| segs.next())
               })
               .map(|id| id.to_string())
        }

        _ => None,
    }
}

async fn write_track_metadata (
    track_info_to_write: TrackInfo
) -> Result<(), Error> {
    println!("Writing out track metadata");
    let video_id = track_info_to_write.id.clone();
    write(format!("media/metadata/{video_id}.json"), serde_json::to_string_pretty(&track_info_to_write)?).expect("Failed to write metadata file");
    Ok(())
}

async fn get_vc_id(ctx: Context<'_>) -> Result<ChannelId, Error> {
    println!("Getting VC id");

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
    println!("Joining user's voice chat");

    let manager = songbird::get(ctx.serenity_context())
        .await
        .expect("Error getting the Songbird client from the manager")
        .clone();

    let join_result = manager.join(guild.id, vc_id).await;
    Ok(join_result?)
}

async fn autocomplete_metadata(
    ctx: Context<'_>,
    partial: &str,
) -> impl Iterator<Item = String> {
    println!("Autocomplete requested: metadata");

    let data: &Data = ctx.data();
    let library = data.library.read().await; // tokio::sync::RwLock

    let needle = partial.to_lowercase();
    let mut choices: HashSet<String> = HashSet::with_capacity(AUTOCOMPLETE_MAX_CHOICES);

    let cmd = ctx.command().name.as_str();

    'outer: for info in library.values() {
        // build a list of candidates for this info
        let candidates: Vec<String> = match cmd {
            "add_tag" => info.tags.clone(),
            "artist" => vec![info.track_artist.clone()],
            "origin" => vec![info.track_origin.clone()],
            _ => Vec::new(),
        };

        // run each candidate through lightweight_trim + filter
        for raw in candidates {
            let display = lightweight_trim(raw);

            if needle.is_empty() || display.to_lowercase().contains(&needle) {
                choices.insert(display);
                if choices.len() >= AUTOCOMPLETE_MAX_CHOICES {
                    break 'outer;
                }
            }
        }
    }

    println!("Choices: {:#?}", choices.clone());
    println!("Command invoking autocomplete: {}", cmd);
    println!("Number of choices: {}", choices.len());
    println!("Search term: {}", partial);

    let mut choices: Vec<String> = choices.into_iter().collect();
    choices.sort_unstable();
    choices.into_iter()
}

async fn autocomplete_track(
    ctx: Context<'_>,
    partial: &str,
) -> impl Iterator<Item = AutocompleteChoice> {
    println!("Autocomplete requested: tracks");

    let data: &Data = ctx.data();
    let library = data.library.read().await; // tokio::sync::RwLock

    let needle = partial.to_lowercase();
    let mut choices: Vec<(String, String)> = Vec::with_capacity(AUTOCOMPLETE_MAX_CHOICES);

    let cmd = ctx.command().name.as_str();

    for info in library.values() {
        let display = build_autocomplete_display(vec![
                info.track_title.clone(),
                info.track_artist.clone(),
                info.track_origin.clone(),
                { if info.tags.len() > 0 { info.tags.join(", ") } else { "No tags".to_string() } }
            ]
        );

        if needle.is_empty() || display.to_lowercase().contains(&needle) {
            choices.push((display, info.id.clone()));
            if choices.len() >= AUTOCOMPLETE_MAX_CHOICES {
                break;
            }
        }
    }
    println!("Choices: {:#?}", choices.clone());
    println!("Command invoking autocomplete: {}", cmd);
    println!("Number of choices: {}", choices.len());
    println!("Search term: {}", partial);

    choices.sort_unstable_by(|(d1, _), (d2, _)| d1.cmp(d2));
    choices
        .into_iter()
        .map(|(display, video_id)| {AutocompleteChoice::new(display, video_id)}
    )
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
    #[autocomplete = "autocomplete_track"]
    track: String
) -> Result<(), Error> {
    let data: &Data = ctx.data();
    let mut library = data.library.write().await; // tokio::sync::RwLock
    // Look up the TrackInfo by key and clear its tags
    if let Some(track_info) = library.get_mut(&track) {
        track_info.tags.clear();
        write_track_metadata(track_info.clone()).await?;
        ctx.say(format!("Reset tags for track `{}`", track_info.track_title)).await?;
    } else {
        ctx.say(format!("No track found with id `{}`", track)).await?;
    }

    Ok(())
}

/// Add a new arbitrary tag to a track
#[poise::command(slash_command)]
pub async fn add_tag(
    ctx: Context<'_>,
    #[description = "The track to add a tag to"]
    #[autocomplete = "autocomplete_track"]
    track: String,
    #[description = "The tag to add"]
    #[autocomplete = "autocomplete_metadata"]
    tag: String
) -> Result<(), Error> {
    let data: &Data = ctx.data();
    let mut library = data.library.write().await; // tokio::sync::RwLock
    // Look up the TrackInfo by key and clear its tags
    if let Some(track_info) = library.get_mut(&track) {
        track_info.tags.push(tag.clone());
        write_track_metadata(track_info.clone()).await?;
        ctx.say(format!("Tag `{}` added to track `{}`", tag, track_info.track_title)).await?;
    } else {
        ctx.say(format!("No track found with id `{}`", track)).await?;
    }

    Ok(())
}

/// Set a track's title, artist, or origin
#[poise::command(slash_command, subcommands("title", "artist", "origin"), subcommand_required)]
pub async fn set_metadata(
    _ctx: Context<'_>,
) -> Result<(), Error> {
    Ok(())
}

/// Set a track's title
#[poise::command(slash_command)]
pub async fn title(
    ctx: Context<'_>,
    #[description = "The track to adjust"]
    #[autocomplete = "autocomplete_track"]
    track: String,
    #[description = "The new title to give the track"]
    new_title: String
) -> Result<(), Error> {
    let data: &Data = ctx.data();
    let mut library = data.library.write().await; // tokio::sync::RwLock
    if let Some(track_info) = library.get_mut(&track) {
        let old_title = track_info.track_title.clone();
        track_info.track_title = new_title.clone();
        write_track_metadata(track_info.clone()).await?;
        ctx.say(format!("Set new title `{}` for track formerly known as `{}`", new_title, old_title)).await?;
    } else {
        ctx.say(format!("No track found with id `{}`", track)).await?;
    }    
    Ok(())
}

/// Set a track's artist
#[poise::command(slash_command)]
pub async fn artist(
    ctx: Context<'_>,
    #[description = "The track to adjust"]
    #[autocomplete = "autocomplete_track"]
    track: String,
    #[description = "The new artist for the track"]
    #[autocomplete = "autocomplete_metadata"]
    new_artist: String
) -> Result<(), Error> {
    let data: &Data = ctx.data();
    let mut library = data.library.write().await; // tokio::sync::RwLock
    if let Some(track_info) = library.get_mut(&track) {
        let track_title = &track_info.track_title;
        let old_artist = track_info.track_artist.clone();
        track_info.track_artist = new_artist.clone();
        write_track_metadata(track_info.clone()).await?;
        ctx.say(format!("Set new artist `{}` for track `{}` (old artist: `{}`)", new_artist, track_title, old_artist)).await?;
    } else {
        ctx.say(format!("No track found with id `{}`", track)).await?;
    }    
    Ok(())
}

/// Set a track's origin (eg. game/movie title)
#[poise::command(slash_command)]
pub async fn origin(
    ctx: Context<'_>,
    #[description = "The track to adjust"]
    #[autocomplete = "autocomplete_track"]
    track: String,
    #[description = "The new origin for the track"]
    #[autocomplete = "autocomplete_metadata"]
    new_origin: String
) -> Result<(), Error> {
    let data: &Data = ctx.data();
    let mut library = data.library.write().await; // tokio::sync::RwLock
    if let Some(track_info) = library.get_mut(&track) {
        let track_title = &track_info.track_title;
        let old_origin = track_info.track_origin.clone();
        track_info.track_origin = new_origin.clone();
        write_track_metadata(track_info.clone()).await?;
        ctx.say(format!("Set new origin `{}` for track `{}` (old origin: `{}`)", new_origin, track_title, old_origin)).await?;
    } else {
        ctx.say(format!("No track found with id `{}`", track)).await?;
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

    let video_id = get_youtube_id(&yt_link).unwrap();

    // outputs the mp3 to:          media/audio/id.extension <- extension should be mp3
    // outputs the info json to:    media/audio/id.info.json
    let output = Command::new("yt-dlp")
        .arg("-t")
        .arg("mp3")
        .arg("-o")
        .arg("media/audio/%(id)s.%(ext)s")
        .arg("--no-playlist")
        .arg("--write-info-json")
        .arg("--no-progress")
        .arg(yt_link)
        .output()
        .expect("Failed to execute yt-dlp");

    println!("{:?}", output);

    // reproceses the file at media/audio/id.info.json
    // and deletes it on the return once the file has been read
    let slim = process_ytdlp_json(video_id).unwrap();

    let track_title = match track_title {
        Some(title) => title,
        None => slim.get("title").and_then(Value::as_str).unwrap().to_string()
    };

    let track_artist = match track_artist {
        Some(artist) => artist,
        None => slim.get("channel").and_then(Value::as_str).unwrap().to_string()
    };

    let track_origin = match track_origin {
        Some(origin) => origin,
        None => "No origin provided".into()
    };

    let new_track = TrackInfo {
        id: slim.get("id").and_then(Value::as_str).unwrap().to_string(),
        upload_date: slim.get("upload_date").and_then(Value::as_str).unwrap().to_string(),
        yt_title: slim.get("title").and_then(Value::as_str).unwrap().to_string(),
        yt_channel: slim.get("channel").and_then(Value::as_str).unwrap().to_string(),
        track_title,
        track_artist,
        track_origin,
        tags: Vec::new(),
    };

    let data: &Data = ctx.data();
    {
        let mut library = data.library.write().await; // tokio::sync::RwLock
        library.insert(
            new_track.id.clone(),
            new_track.clone()
        );
    }

    write_track_metadata(new_track).await?;

    let title = slim.get("title").and_then(Value::as_str).unwrap().to_string();
    ctx.say(format!("File downloaded: `{title}`")).await?;
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
    #[autocomplete = "autocomplete_track"]
    track: String
) -> Result<(), Error> {

    let guild = ctx.guild().expect("Must be in a guild to use voice").clone();
    let vc_id = get_vc_id(ctx).await?;

    let serenity_ctx = ctx.serenity_context();

    let manager = songbird::get(serenity_ctx)
        .await
        .expect("Songbird was not initialized")
        .clone();

    join_vc(ctx, guild.clone(), vc_id).await?;
    let track_path = format!("media/audio/{track}.mp3");
    println!("{}", track_path.clone());

    let path = std::env::current_dir()?;
    println!("The current directory is {}", path.display());

    let song_src = Compressed::new(
        SongbirdFile::new(track_path).into(),
        Bitrate::BitsPerSecond(128_000),
    )
        .await
        .expect("An error occurred constructing the track source");
    let _ = song_src.raw.spawn_loader();

    let data: &Data = ctx.data();
    let library = data.library.read().await;

    if let Some(handler_lock) = manager.get(guild.id.clone()) {
        let mut handler = handler_lock.lock().await;
        let track_handle = handler.play_only_input(song_src.into());
        let _ = track_handle.enable_loop()?;
        let mut handles = data.track_handles.write().await; // tokio::sync::RwLock
        handles.insert(
            guild.id,
            track_handle
        );
    }

    ctx.say(format!("Playing selected track: `{}`", library.get(&track).unwrap().track_title)).await?;

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

#[poise::command(slash_command, prefix_command)]
pub async fn paginate(ctx: Context<'_>) -> Result<(), Error> {
    let pages = &[
        "`Content of first page`",
        "`Content of second page`",
        "`Content of third page`",
        "`Content of fourth page`",
    ];

    poise::samples::paginate(ctx, pages).await?;

    Ok(())
}