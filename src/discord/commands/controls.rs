use crate::{discord::{autocomplete::autocomplete_track, context::{Error, PoiseContext}, voice::{ensure_vc, leave_vc, require_guild}}, track::resolver::resolve_track};

/// Joins your voice channel
#[poise::command(slash_command)]
pub async fn join(ctx: PoiseContext<'_>) -> Result<(), Error> {
    ensure_vc(ctx).await?;
    ctx.say("Joined your voice channel! 🎶").await?;
    Ok(())
}

/// Plays a selected track from the library
#[poise::command(slash_command)]
pub async fn play(
    ctx: PoiseContext<'_>,
    #[description = "Track to play now"]
    #[autocomplete = "autocomplete_track"]
    track: String,
) -> Result<(), Error> {
    let (guild_id, call) = ensure_vc(ctx).await?;
    let track_info = resolve_track(&ctx.data().db_pool, track).await?;

    ctx.data()
        .player
        .play(guild_id, call, track_info.clone())
        .await?;

    ctx.say(format!(
        "Now playing: `{}` by `{}`, from `{}`.",
        track_info.title,
        track_info.artist,
        track_info.origin,
    )).await?;

    Ok(())
}

/// Displays the currently playing track's details
#[poise::command(slash_command)]
pub async fn now_playing(ctx: PoiseContext<'_>) -> Result<(), Error> {
    let guild_id = require_guild(ctx)?;

    match ctx.data().player.get_now_playing(guild_id).await {
        Some(track) => {
            ctx.say(format!(
                "Now Playing:\n**Title:** {}\n**Artist:** {}\n**Origin:** {}",
                track.title, track.artist, track.origin,
            )).await?;
        }
        None => {
            ctx.say("No track is currently playing.").await?;
        }
    }

    Ok(())
}

/// Loop or un-loop the currently playing track
#[poise::command(slash_command, prefix_command)]
pub async fn loop_track(ctx: PoiseContext<'_>) -> Result<(), Error> {
    let guild_id = require_guild(ctx)?;
    let _track = ctx.data().player.require_now_playing(guild_id).await?;
    let looping = ctx.data().player.toggle_loop(guild_id).await?;
    ctx.say(format!(
        "Looping {}",
        if looping { "enabled" } else { "disabled" }
    )).await?;
    Ok(())
}

/// Toggles pause/unpause for the currently playing track
#[poise::command(slash_command)]
pub async fn pause(ctx: PoiseContext<'_>) -> Result<(), Error> {
    let guild_id = require_guild(ctx)?;
    let _track = ctx.data().player.require_now_playing(guild_id).await?;
    let playing = ctx.data().player.pause(guild_id).await?;
    ctx.say(if playing {
        "Resumed the currently paused track."
    } else {
        "Paused the currently playing track."
    }).await?;
    Ok(())
}

/// Leaves the voice channel
#[poise::command(slash_command)]
pub async fn leave(ctx: PoiseContext<'_>) -> Result<(), Error> {
    let guild_id = require_guild(ctx)?;

    leave_vc(ctx, guild_id).await?;

    ctx.data().player.clear_now_playing(guild_id).await;
    ctx.say("Left the voice channel.").await?;

    Ok(())
}