////////////////////////////////////////////////////////////////////////////////
// Imports

use songbird::input::File as SongbirdFile;
use songbird::input::cached::Compressed;
use songbird::driver::Bitrate;
use songbird::tracks::LoopState;
use crate::definitions::{Context, Error, Data};
use crate::library::{get_vc_id, join_vc, get_youtube_id};
use crate::autocomplete::*;
use crate::cmd_management::download_direct;

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


pub async fn play_direct(
    ctx: Context<'_>,
    track_ref: String,
) -> Result<(), Error> {
    let guild = ctx.guild().expect("Must be in a guild to use voice").clone();
    let vc_id = get_vc_id(ctx).await?;

    let serenity_ctx = ctx.serenity_context();

    let manager = songbird::get(serenity_ctx)
        .await
        .expect("Songbird was not initialized")
        .clone();

    join_vc(ctx, guild.clone(), vc_id).await?;

    let track_path = format!("audio/{track_ref}.mp3");

    println!("{}", track_path);

    let path = std::env::current_dir()?;
    println!("The current directory is {}", path.display());

    let song_src = Compressed::new(
        SongbirdFile::new(track_path).into(),
        Bitrate::Bits(128_000),
    )
    .await
    .expect("An error occurred constructing the track source");

    let _ = song_src.raw.spawn_loader();

    let data: &Data = ctx.data();

    if let Some(handler_lock) = manager.get(guild.id) {
        let mut handler = handler_lock.lock().await;

        let track_handle = handler.play_only_input(song_src.into());

        let _ = track_handle.enable_loop()?;

        {
            let mut track_metadata = data.track_metadata.write().await;
            track_metadata.insert(guild.id, track_ref.clone());
        }

        let mut handles = data.track_handles.write().await;
        handles.insert(guild.id, track_handle);
    }

    Ok(())
}

/// Plays a selected track from the library
#[poise::command(slash_command)]
pub async fn play(
    ctx: Context<'_>,
    #[description = "Track to play now"]
    #[autocomplete = "autocomplete_track"]
    track: String, 
    // track here actually refers to a youtube id but discord exposes this
    // variable as the argument's name due to the autocorrect implementation
    // makes much more sense on user-end to name it track
) -> Result<(), Error> {
    // immediately fix the above comment's note so the code is clearer
    let track_ref = track;

    let db_pool = &ctx.data().db_pool;

    // Normalize input early (URL or raw ID → YouTube ID)
    let lookup_id = get_youtube_id(&track_ref)
        .unwrap_or_else(|| track_ref.clone());

    // Try DB lookup using normalized ID
    let track_metadata: Option<(String, String)> = sqlx::query_as(
        "SELECT track_title, artists.artist FROM tracks
         LEFT JOIN artists ON tracks.artist_id = artists.id
         WHERE tracks.id = ?1",
    )
    .bind(&lookup_id)
    .fetch_optional(db_pool)
    .await?;

    let track_ref = match track_metadata {
        Some((track_title, track_artist)) => {
            ctx.say(format!(
                "Now playing: `{}` by `{}`",
                track_title, track_artist
            ))
            .await?;

            // Already downloaded/local
            lookup_id
        }
        None => {
            ctx.say(format!(
                "Track `{}` not found locally. Downloading...",
                lookup_id
            ))
            .await?;

            // Download using original input (URL or ID)
            download_direct(ctx, track_ref, None, None, None).await?
        }
    };

    // Single unified playback path
    play_direct(ctx, track_ref).await?;

    Ok(())
}

/// Displays the currently playing track's details
#[poise::command(slash_command)]
pub async fn now_playing(
    ctx: Context<'_>,
) -> Result<(), Error> {
    // Ensure the command is used in a guild
    let guild_id = if let Some(g) = ctx.guild_id() {
        g
    } else {
        return Err("This command can only be used in a server.".into());
    };

    // Access the track metadata for the current guild
    let data: &Data = ctx.data();
    let track_metadata = data.track_metadata.read().await; // tokio::sync::RwLock
    if let Some(track_id) = track_metadata.get(&guild_id) {
        let db_pool = &ctx.data().db_pool;

        // Query the database for track details
        let track_details: Option<(String, String, String, Option<String>)> = sqlx::query_as(
            "SELECT tracks.track_title, artists.artist, origins.origin,
                    GROUP_CONCAT(tags.tag, ', ') AS tags
            FROM tracks
            LEFT JOIN artists ON tracks.artist_id = artists.id
            LEFT JOIN origins ON tracks.origin_id = origins.id
            LEFT JOIN track_tags ON tracks.id = track_tags.track_id
            LEFT JOIN tags ON track_tags.tag_id = tags.id
            WHERE tracks.id = ?1
            GROUP BY tracks.id, artists.artist, origins.origin"
        )
        .bind(track_id)
        .fetch_optional(db_pool)
        .await?;

        if let Some((title, artist, origin, tags)) = track_details {
            ctx.say(format!(
                "Now Playing:\n**Title:** {}\n**Artist:** {}\n**Origin:** {}\n**Tags:** {}",
                title,
                artist,
                origin,
                tags.unwrap_or_else(|| "None".to_string())
            ))
            .await?;
        } else {
            ctx.say("No details found for the currently playing track.").await?;
        }
    } else {
        ctx.say("No track is currently playing.").await?;
    }

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

/// Toggles pause/unpause for the currently playing track
#[poise::command(slash_command)]
pub async fn pause(
    ctx: Context<'_>,
) -> Result<(), Error> {
    // Ensure the command is used in a guild
    let guild_id = if let Some(g) = ctx.guild_id() {
        g
    } else {
        return Err("Pause command can only be used in a server.".into());
    };

    // Access the track handle for the current guild
    let data: &Data = ctx.data();
    let handles = data.track_handles.read().await; // tokio::sync::RwLock
    if let Some(track_handle) = handles.get(&guild_id) {
        let handle_info = track_handle.clone().get_info().await?;
        if handle_info.playing == songbird::tracks::PlayMode::Play {
            track_handle.pause()?;
            ctx.say("Paused the currently playing track.").await?;
        } else {
            track_handle.play()?;
            ctx.say("Resumed the currently paused track.").await?;
        }
    } else {
        ctx.say("No track is currently playing.").await?;
    }

    Ok(())
}