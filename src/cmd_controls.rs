////////////////////////////////////////////////////////////////////////////////
// Imports

use songbird::input::File as SongbirdFile;
use songbird::input::cached::Compressed;
use songbird::driver::Bitrate;
use songbird::tracks::LoopState;
use crate::definitions::{Context, Error, Data, NowPlaying, TrackInfo, VideoId};
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
    track_info: TrackInfo,
) -> Result<(), Error> {
    let guild = ctx.guild().expect("Must be in a guild to use voice").clone();
    let vc_id = get_vc_id(ctx).await?;

    let serenity_ctx = ctx.serenity_context();

    let manager = songbird::get(&serenity_ctx)
        .await
        .expect("Songbird was not initialized")
        .clone();

    join_vc(ctx, guild.clone(), vc_id).await?;

    let track_path = {
        let track_id_str = track_info.id.as_str();
        format!("audio/{track_id_str}.mp3")
    };

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
            let mut state = data.now_playing.write().await;

            state.insert(
                guild.id,
                NowPlaying {
                    track: track_info,
                    handle: track_handle,
                },
            );
        }
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
    let track_metadata: Option<(String, String, String)> = sqlx::query_as(
        "SELECT tracks.track_title,
                artists.artist,
                origins.origin
        FROM tracks
        LEFT JOIN artists ON tracks.artist_id = artists.id
        LEFT JOIN origins ON tracks.origin_id = origins.id
        WHERE tracks.id = ?1",
    )
    .bind(&lookup_id)
    .fetch_optional(db_pool)
    .await?;

    let track_info = match track_metadata {
        Some((title, artist, origin)) => {
            TrackInfo {
                id: VideoId::from(lookup_id.clone()),
                title,
                artist,
                origin,
            }
        }

        None => {
            ctx.say(format!(
                "Track `{}` not found locally. Downloading...",
                lookup_id
            ))
            .await?;

            let (track_id, title, artist) =
                download_direct(ctx, track_ref, None, None, None).await?;

            TrackInfo {
                id: VideoId::from(track_id),
                title,
                artist,
                origin: "Unknown".to_string(), // see note below
            }
        }
    };

    // Single unified playback path
    play_direct(ctx, track_info.clone()).await?;

    ctx.say(format!(
        "Now playing: `{}` by `{}`",
        track_info.title,
        track_info.artist
    ))
    .await?;

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

    let data: &Data = ctx.data();
    let state = data.now_playing.read().await;

    if let Some(now) = state.get(&guild_id) {
        let track = &now.track;

        ctx.say(format!(
            "Now Playing:\n**Title:** {}\n**Artist:** {}\n**Origin:** {}",
            track.title,
            track.artist,
            track.origin
        ))
        .await?;
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
    let guild_id = if let Some(g) = ctx.guild_id() {
        g
    } else {
        return Err("Looping only works in a server".into());
    };

    let data: &Data = ctx.data();
    let state = data.now_playing.read().await;

    if let Some(now) = state.get(&guild_id) {
        let handle = &now.handle;

        let handle_info = handle.clone().get_info().await?;
        let loops = handle_info.loops;

        let new_state = match loops {
            LoopState::Infinite => {
                let _ = handle.disable_loop()?;
                false
            }
            LoopState::Finite(_) => {
                let _ = handle.enable_loop()?;
                true
            }
        };

        ctx.say(format!(
            "Looping {}",
            if new_state { "enabled" } else { "disabled" }
        ))
        .await?;
    } else {
        ctx.say("No track is currently playing.").await?;
    }

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
    let guild_id = if let Some(g) = ctx.guild_id() {
        g
    } else {
        return Err("Pause command can only be used in a server.".into());
    };

    let data: &Data = ctx.data();
    let state = data.now_playing.read().await;

    if let Some(now) = state.get(&guild_id) {
        let handle = &now.handle;

        let handle_info = handle.clone().get_info().await?;

        if handle_info.playing == songbird::tracks::PlayMode::Play {
            handle.pause()?;
            ctx.say("Paused the currently playing track.").await?;
        } else {
            handle.play()?;
            ctx.say("Resumed the currently paused track.").await?;
        }
    } else {
        ctx.say("No track is currently playing.").await?;
    }

    Ok(())
}