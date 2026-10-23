use std::fmt::Write;

use crate::{
    discord::{
        autocomplete::{autocomplete_mix_filter, autocomplete_queue_position, autocomplete_track},
        context::{Error, PoiseContext},
        format::lightweight_trim,
        voice::{ensure_vc, leave_vc, require_guild},
    },
    jester::{
        db::{MIX_LIMIT, MixFilter, fetch_mix_tracks, parse_filter},
        db::{get_break_track, set_break_track},
        player::{HistoryOutcome, RepeatMode},
        track::{TrackInfo, resolve_track},
    },
};

pub fn pause_message(resumed: bool) -> &'static str {
    if resumed {
        "Resumed the currently paused track."
    } else {
        "Paused the currently playing track."
    }
}

fn play_message(track: &TrackInfo) -> String {
    format!(
        "Now playing: `{}` by `{}`, from `{}`.",
        track.title,
        track.artist.as_deref().unwrap_or("unknown artist"),
        track.origin.as_deref().unwrap_or("unknown origin")
    )
}

fn now_playing_message(track: Option<&TrackInfo>) -> String {
    track.map_or_else(
        || "No track is currently playing.".into(),
        |track| {
            format!(
                "Now Playing:\n**Title:** {}\n**Artist:** {}\n**Origin:** {}",
                track.title,
                track.artist.as_deref().unwrap_or("unknown artist"),
                track.origin.as_deref().unwrap_or("unknown origin")
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
    let track_info =
        resolve_track(&ctx.data().db_pool, track, ctx.data().downloader.clone()).await?;
    ctx.data()
        .player
        .play_now(guild_id, call, track_info.clone())
        .await?;
    ctx.say(play_message(&track_info)).await?;
    Ok(())
}

/// Creates a shuffled mix matching any selected tag.
#[poise::command(slash_command)]
pub async fn mix(
    ctx: PoiseContext<'_>,
    #[description = "Optional comma-separated tags; tracks matching any tag are included"]
    #[autocomplete = "autocomplete_mix_filter"]
    tags: Option<String>,
) -> Result<(), Error> {
    let filter = MixFilter {
        tags: parse_filter(tags.as_deref())?,
    };
    let tracks = fetch_mix_tracks(&ctx.data().db_pool, &filter, MIX_LIMIT).await?;
    if tracks.is_empty() {
        ctx.say("No tracks matched those filters; the current queue was unchanged.")
            .await?;
        return Ok(());
    }

    let (guild_id, _, call) = ensure_vc(ctx).await?;
    ctx.data().player.clear_queue(guild_id).await;
    let count = tracks.len();
    let next_track = tracks
        .first()
        .map(|track| {
            format!(
                "`{}` by `{}`",
                track.title,
                track.artist.as_deref().unwrap_or("unknown artist")
            )
        })
        .ok_or("Mix unexpectedly contained no tracks.")?;
    for track in tracks {
        ctx.data()
            .player
            .enqueue(guild_id, call.clone(), track, false)
            .await?;
    }

    ctx.say(format!(
        "Created a shuffled mix of {count} track(s); replaced the upcoming queue.\nNext track: {next_track}."
    ))
    .await?;
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
    let track_info =
        resolve_track(&ctx.data().db_pool, track, ctx.data().downloader.clone()).await?;
    let started = ctx
        .data()
        .player
        .enqueue(guild_id, call, track_info.clone(), false)
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
    let track_info =
        resolve_track(&ctx.data().db_pool, track, ctx.data().downloader.clone()).await?;
    let started = ctx
        .data()
        .player
        .enqueue(guild_id, call, track_info.clone(), true)
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
            HistoryOutcome::Failed => "failed",
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
    let guild_id = require_guild(ctx)?;
    let track = ctx.data().player.get_now_playing(guild_id).await;
    let mut message = now_playing_message(track.as_ref());
    if ctx.data().player.break_active(guild_id).await {
        message.push_str("\n**Break:** active");
    }
    ctx.say(message).await?;
    Ok(())
}

/// Configure personal break music or toggle the server's shared break state.
#[poise::command(
    slash_command,
    rename = "break",
    subcommands("break_toggle", "break_set"),
    subcommand_required
)]
#[allow(clippy::unused_async)]
pub async fn break_cmd(_ctx: PoiseContext<'_>) -> Result<(), Error> {
    Ok(())
}

#[poise::command(slash_command, rename = "toggle")]
async fn break_toggle(ctx: PoiseContext<'_>) -> Result<(), Error> {
    if !ctx.data().config.is_chronicle_gm(ctx.author().id) {
        return Err("Only a registered GM can toggle a break.".into());
    }
    let guild_id = require_guild(ctx)?;
    let is_active = ctx.data().player.break_active(guild_id).await;
    let (call, track) = if is_active {
        // The toggle-off path ignores the selected GM's track.
        (
            None,
            ctx.data()
                .player
                .get_now_playing(guild_id)
                .await
                .ok_or("No track is currently playing.")?,
        )
    } else {
        let (_, _, call) = ensure_vc(ctx).await?;
        (
            Some(call),
            get_break_track(&ctx.data().db_pool, ctx.author().id)
                .await?
                .ok_or("Set your break music first with `/break set`.")?,
        )
    };
    let started = ctx
        .data()
        .player
        .toggle_break(guild_id, call, track.clone())
        .await?;
    ctx.say(if started {
        format!("Break started with `{}`.", track.title)
    } else {
        "Break ended; resuming the previous track.".to_owned()
    })
    .await?;
    Ok(())
}

#[poise::command(slash_command, rename = "set")]
async fn break_set(
    ctx: PoiseContext<'_>,
    #[description = "Track to use for your breaks"]
    #[autocomplete = "autocomplete_track"]
    track: String,
) -> Result<(), Error> {
    if !ctx.data().config.is_chronicle_gm(ctx.author().id) {
        return Err("Only a registered GM can set break music.".into());
    }
    require_guild(ctx)?;
    let track = resolve_track(&ctx.data().db_pool, track, ctx.data().downloader.clone()).await?;
    set_break_track(&ctx.data().db_pool, ctx.author().id, &track).await?;
    ctx.say(format!(
        "Your break music is set to `{}`. It will take effect the next time a break starts.",
        track.title
    ))
    .await?;
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
