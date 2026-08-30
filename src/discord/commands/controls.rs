use crate::{
    discord::{
        autocomplete::autocomplete_track,
        context::{Error, PoiseContext},
        voice::{ensure_vc, leave_vc, require_guild},
    },
    jester::track::resolver::resolve_track,
};
use tracing::info;

pub fn play_message(track: &crate::jester::track::types::TrackInfo) -> String {
    format!(
        "Now playing: `{}` by `{}`, from `{}`.",
        track.title, track.artist, track.origin
    )
}
pub fn now_playing_message(track: Option<&crate::jester::track::types::TrackInfo>) -> String {
    track.map_or_else(
        || "No track is currently playing.".into(),
        |track| {
            format!(
                "Now Playing:\n**Title:** {}\n**Artist:** {}\n**Origin:** {}",
                track.title, track.artist, track.origin
            )
        },
    )
}
pub fn toggle_message(enabled: bool) -> String {
    format!("Looping {}", if enabled { "enabled" } else { "disabled" })
}
pub fn pause_message(resumed: bool) -> &'static str {
    if resumed {
        "Resumed the currently paused track."
    } else {
        "Paused the currently playing track."
    }
}

/// Joins your voice channel
#[poise::command(slash_command)]
pub async fn join(ctx: PoiseContext<'_>) -> Result<(), Error> {
    info!(user = %ctx.author().id, "Join command requested");
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
    info!(user = %ctx.author().id, "Play command requested");
    let (guild_id, _, call) = ensure_vc(ctx).await?;
    let track_info = resolve_track(&ctx.data().db_pool, track).await?;

    ctx.data()
        .player
        .play(guild_id, call, track_info.clone())
        .await?;

    ctx.say(play_message(&track_info)).await?;

    Ok(())
}

/// Displays the currently playing track's details
#[poise::command(slash_command)]
pub async fn now_playing(ctx: PoiseContext<'_>) -> Result<(), Error> {
    info!(user = %ctx.author().id, "Now-playing command requested");
    let guild_id = require_guild(ctx)?;

    match ctx.data().player.get_now_playing(guild_id).await {
        Some(track) => {
            ctx.say(now_playing_message(Some(&track))).await?;
        }
        None => {
            ctx.say(now_playing_message(None)).await?;
        }
    }

    Ok(())
}

/// Loop or un-loop the currently playing track
#[poise::command(slash_command, prefix_command)]
pub async fn loop_track(ctx: PoiseContext<'_>) -> Result<(), Error> {
    info!(user = %ctx.author().id, "Loop command requested");
    let guild_id = require_guild(ctx)?;
    let _track = ctx.data().player.require_now_playing(guild_id).await?;
    let looping = ctx.data().player.toggle_loop(guild_id).await?;
    ctx.say(toggle_message(looping)).await?;
    Ok(())
}

/// Toggles pause/unpause for the currently playing track
#[poise::command(slash_command)]
pub async fn pause(ctx: PoiseContext<'_>) -> Result<(), Error> {
    info!(user = %ctx.author().id, "Pause command requested");
    let guild_id = require_guild(ctx)?;
    let _track = ctx.data().player.require_now_playing(guild_id).await?;
    let playing = ctx.data().player.pause(guild_id).await?;
    ctx.say(pause_message(playing)).await?;
    Ok(())
}

/// Leaves the voice channel
#[poise::command(slash_command)]
pub async fn leave(ctx: PoiseContext<'_>) -> Result<(), Error> {
    info!(user = %ctx.author().id, "Leave command requested");
    let guild_id = require_guild(ctx)?;

    leave_vc(ctx, guild_id).await?;

    ctx.data().player.clear_now_playing(guild_id).await;
    ctx.say("Left the voice channel.").await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{now_playing_message, pause_message, play_message, toggle_message};
    use crate::jester::track::types::{TrackInfo, VideoId};

    fn track() -> TrackInfo {
        TrackInfo {
            id: VideoId::from("id"),
            title: "Title".into(),
            artist: "Artist".into(),
            origin: "Origin".into(),
        }
    }

    #[test]
    fn formats_play_message() {
        assert_eq!(
            play_message(&track()),
            "Now playing: `Title` by `Artist`, from `Origin`."
        );
    }

    #[test]
    fn formats_now_playing_for_present_and_absent_tracks() {
        assert_eq!(
            now_playing_message(Some(&track())),
            "Now Playing:\n**Title:** Title\n**Artist:** Artist\n**Origin:** Origin"
        );
        assert_eq!(now_playing_message(None), "No track is currently playing.");
    }

    #[test]
    fn formats_loop_and_pause_state_changes() {
        assert_eq!(toggle_message(true), "Looping enabled");
        assert_eq!(toggle_message(false), "Looping disabled");
        assert_eq!(pause_message(true), "Resumed the currently paused track.");
        assert_eq!(pause_message(false), "Paused the currently playing track.");
    }
}
