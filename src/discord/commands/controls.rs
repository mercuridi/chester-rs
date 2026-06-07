use crate::definitions::{PoiseContext, Error};
use crate::utils::context::{get_vc_id, join_vc, require_guild};
use crate::discord::autocomplete::autocomplete_track;
use crate::utils::track_resolver::resolve_track;

/// Joins your voice channel
#[poise::command(slash_command)]
pub async fn join(ctx: PoiseContext<'_>) -> Result<(), Error> {
    let guild = ctx.guild().ok_or("Must be in a guild")?.clone();
    let vc_id = get_vc_id(ctx).await?;
    join_vc(ctx, guild, vc_id).await?;
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
    let guild_id = require_guild(ctx)?;
    let vc_id = get_vc_id(ctx).await?;
    let track_info = resolve_track(&ctx.data().db_pool, track).await?;

    ctx.data().player.play(
        guild_id,
        vc_id,
        track_info.clone(),
        ctx.serenity_context(),
    ).await?;

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
    let _ = get_vc_id(ctx).await?; // verify user is in a voice channel

    ctx.data().player.leave(guild_id, ctx.serenity_context()).await?;

    ctx.say("Left the voice channel.").await?;
    Ok(())
}