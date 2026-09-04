use crate::{
    discord::{
        autocomplete::{autocomplete_queue_position, autocomplete_track},
        context::{Error, PoiseContext},
        voice::{ensure_vc, leave_vc, require_guild},
    },
    jester::{
        player::queue::{HistoryOutcome, RepeatMode},
        track::resolver::resolve_track,
    },
    utils::format::lightweight_trim,
};
use std::fmt::Write;
pub fn pause_message(resumed: bool) -> &'static str {
    if resumed {
        "Resumed the currently paused track."
    } else {
        "Paused the currently playing track."
    }
}

fn play_message(track: &crate::jester::track::types::TrackInfo) -> String {
    format!(
        "Now playing: `{}` by `{}`, from `{}`.",
        track.title, track.artist, track.origin
    )
}

fn now_playing_message(track: Option<&crate::jester::track::types::TrackInfo>) -> String {
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

/// Joins your voice channel.
#[poise::command(slash_command)]
pub async fn join(ctx: PoiseContext<'_>) -> Result<(), Error> {
    ensure_vc(ctx).await?;
    ctx.say("Joined your voice channel! 🎶").await?;
    Ok(())
}

/// Immediately plays a track, replacing the current track while preserving the queue.
#[poise::command(slash_command)]
pub async fn play(
    ctx: PoiseContext<'_>,
    #[description = "Track to play immediately"]
    #[autocomplete = "autocomplete_track"]
    track: String,
) -> Result<(), Error> {
    let (guild_id, _, call) = ensure_vc(ctx).await?;
    let track_info = resolve_track(&ctx.data().db_pool, track).await?;
    ctx.data()
        .player
        .play_now(guild_id, call, track_info.clone())
        .await?;
    ctx.say(play_message(&track_info)).await?;
    Ok(())
}

/// Shows the active track and explicit upcoming queue.
#[poise::command(
    slash_command,
    subcommands(
        "queue_add",
        "queue_next",
        "queue_remove",
        "queue_move",
        "queue_clear",
        "queue_shuffle",
        "queue_show",
    ),
    subcommand_required
)]
#[allow(clippy::unused_async)] // Poise command handlers are async, even for a subcommand-only group.
pub async fn queue(_ctx: PoiseContext<'_>) -> Result<(), Error> {
    Ok(())
}

#[poise::command(slash_command, rename = "show")]
pub async fn queue_show(ctx: PoiseContext<'_>) -> Result<(), Error> {
    let guild_id = require_guild(ctx)?;
    let snapshot = ctx.data().player.queue_snapshot(guild_id).await;
    let mut message = match snapshot.current {
        Some(item) => format!("**Now playing:** {}", item.track.title),
        None => "**Now playing:** nothing".into(),
    };
    let repeat = match snapshot.repeat_mode {
        RepeatMode::Off => "off",
        RepeatMode::Track => "track",
        RepeatMode::Queue => "queue",
    };
    write!(message, "\n**Repeat:** {repeat}")?;
    message.push_str("\n**Up next:**");
    if snapshot.upcoming.is_empty() {
        message.push_str("\n*(empty)*");
    }
    for (index, entry) in snapshot.upcoming.iter().enumerate() {
        write!(message, "\n{}. {}", index + 1, entry.track.title)?;
    }
    ctx.say(message).await?;
    Ok(())
}

#[poise::command(slash_command, rename = "add")]
pub async fn queue_add(
    ctx: PoiseContext<'_>,
    #[autocomplete = "autocomplete_track"] track: String,
) -> Result<(), Error> {
    let (guild_id, _, call) = ensure_vc(ctx).await?;
    let track_info = resolve_track(&ctx.data().db_pool, track).await?;
    let started = ctx
        .data()
        .player
        .enqueue(guild_id, call, track_info.clone(), ctx.author().id, false)
        .await?;
    ctx.say(if started {
        play_message(&track_info)
    } else {
        format!("Added `{}` to the queue.", track_info.title)
    })
    .await?;
    Ok(())
}

#[poise::command(slash_command, rename = "next")]
pub async fn queue_next(
    ctx: PoiseContext<'_>,
    #[autocomplete = "autocomplete_track"] track: String,
) -> Result<(), Error> {
    let (guild_id, _, call) = ensure_vc(ctx).await?;
    let track_info = resolve_track(&ctx.data().db_pool, track).await?;
    let started = ctx
        .data()
        .player
        .enqueue(guild_id, call, track_info.clone(), ctx.author().id, true)
        .await?;
    ctx.say(if started {
        play_message(&track_info)
    } else {
        format!("Added `{}` as the next track.", track_info.title)
    })
    .await?;
    Ok(())
}

#[poise::command(slash_command, rename = "remove")]
pub async fn queue_remove(
    ctx: PoiseContext<'_>,
    #[autocomplete = "autocomplete_queue_position"] position: String,
) -> Result<(), Error> {
    let position: usize = position
        .parse()
        .map_err(|_| "Choose a queue entry from the autocomplete list.")?;
    let track = ctx
        .data()
        .player
        .remove_queue_entry(require_guild(ctx)?, position)
        .await?;
    ctx.say(format!("Removed `{}` from the queue.", track.title))
        .await?;
    Ok(())
}

#[poise::command(slash_command, rename = "move")]
pub async fn queue_move(
    ctx: PoiseContext<'_>,
    #[min = 1] from: usize,
    #[min = 1] to: usize,
) -> Result<(), Error> {
    ctx.data()
        .player
        .move_queue_entry(require_guild(ctx)?, from, to)
        .await?;
    ctx.say("Moved queue entry.").await?;
    Ok(())
}

#[poise::command(slash_command, rename = "clear")]
pub async fn queue_clear(ctx: PoiseContext<'_>) -> Result<(), Error> {
    ctx.data().player.clear_queue(require_guild(ctx)?).await;
    ctx.say("Cleared upcoming tracks.").await?;
    Ok(())
}

#[poise::command(slash_command, rename = "shuffle")]
pub async fn queue_shuffle(ctx: PoiseContext<'_>) -> Result<(), Error> {
    ctx.data().player.shuffle_queue(require_guild(ctx)?).await;
    ctx.say("Shuffled upcoming tracks.").await?;
    Ok(())
}

/// Skips to the next queued song.
#[poise::command(slash_command)]
pub async fn skip(ctx: PoiseContext<'_>) -> Result<(), Error> {
    let track = ctx.data().player.skip(require_guild(ctx)?).await?;
    ctx.say(format!("Skipped to `{}`.", track.title)).await?;
    Ok(())
}

/// Shows recently completed, skipped, and replaced tracks.
#[poise::command(slash_command)]
pub async fn history(ctx: PoiseContext<'_>, #[min = 1] page: Option<usize>) -> Result<(), Error> {
    const PAGE_SIZE: usize = 10;

    let entries = ctx.data().player.history(require_guild(ctx)?).await;
    if entries.is_empty() {
        ctx.say("No playback history for this server yet.").await?;
        return Ok(());
    }

    let page = page.unwrap_or(1);
    let page_count = entries.len().div_ceil(PAGE_SIZE);
    if page > page_count {
        return Err(format!("History has only {page_count} page(s).").into());
    }

    let start = (page - 1) * PAGE_SIZE;
    let mut message = format!("**Playback history — page {page}/{page_count}:**");
    for entry in entries.iter().rev().skip(start).take(PAGE_SIZE) {
        let outcome = match entry.outcome {
            HistoryOutcome::Completed => "completed",
            HistoryOutcome::Skipped => "skipped",
            HistoryOutcome::Replaced => "replaced",
        };
        write!(
            message,
            "\n- `{}` — {outcome}",
            lightweight_trim(entry.item.track.title.clone(), 120)
        )?;
    }
    ctx.say(message).await?;
    Ok(())
}

/// Sets repeat mode: off, track, or queue.
#[poise::command(slash_command, rename = "loop")]
pub async fn loop_track(
    ctx: PoiseContext<'_>,
    #[description = "off, track, or queue"] mode: String,
) -> Result<(), Error> {
    let mode = match mode.to_ascii_lowercase().as_str() {
        "off" => RepeatMode::Off,
        "track" => RepeatMode::Track,
        "queue" => RepeatMode::Queue,
        _ => return Err("Loop mode must be `off`, `track`, or `queue`.".into()),
    };
    ctx.data()
        .player
        .set_repeat_mode(require_guild(ctx)?, mode)
        .await;
    ctx.say(format!(
        "Repeat mode set to `{}`.",
        match mode {
            RepeatMode::Off => "off",
            RepeatMode::Track => "track",
            RepeatMode::Queue => "queue",
        }
    ))
    .await?;
    Ok(())
}

#[poise::command(slash_command)]
pub async fn now_playing(ctx: PoiseContext<'_>) -> Result<(), Error> {
    let track = ctx.data().player.get_now_playing(require_guild(ctx)?).await;
    ctx.say(now_playing_message(track.as_ref())).await?;
    Ok(())
}

#[poise::command(slash_command)]
pub async fn pause(ctx: PoiseContext<'_>) -> Result<(), Error> {
    let playing = ctx.data().player.pause(require_guild(ctx)?).await?;
    ctx.say(pause_message(playing)).await?;
    Ok(())
}

#[poise::command(slash_command)]
pub async fn leave(ctx: PoiseContext<'_>) -> Result<(), Error> {
    let guild_id = require_guild(ctx)?;
    leave_vc(ctx, guild_id).await?;
    ctx.data().player.clear_now_playing(guild_id).await;
    ctx.say("Left the voice channel.").await?;
    Ok(())
}
