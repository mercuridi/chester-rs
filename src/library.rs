use crate::constants::{ELLIPSIS, ELLIPSIS_LEN};
use crate::definitions::{PoiseContext, Error};

use songbird::Call;
use tokio::sync::Mutex;
use poise::serenity_prelude::{ChannelId, Guild, GuildId};
use url::Url;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::fs;

pub fn process_ytdlp_json(
    file_id: String
) -> Result<serde_json::Value> {
    let path = format!("audio/{file_id}.info.json");
    let content = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read {:?}", path))?;

    // Parse the full JSON
    let v: Value = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse JSON from {:?}", path))?;

    // Extract only the fields we want
    let slim = json!({
        "id": v.get("id").cloned().ok_or_else(|| anyhow::anyhow!("Missing 'id' field in yt-dlp JSON"))?,
        "upload_date": v.get("upload_date").cloned().ok_or_else(|| anyhow::anyhow!("Missing 'upload_date' field in yt-dlp JSON"))?,
        "title": v.get("title").cloned().ok_or_else(|| anyhow::anyhow!("Missing 'title' field in yt-dlp JSON"))?,
        "channel": v.get("channel").cloned().ok_or_else(|| anyhow::anyhow!("Missing 'channel' field in yt-dlp JSON"))?,
    });

    fs::remove_file(&path).ok();

    Ok(slim)
}

pub fn lightweight_trim(mut choice: String, max_width: usize) -> String {
    if max_width <= ELLIPSIS_LEN {
        return ELLIPSIS.to_string();
    }

    if choice.len() > max_width {
        let cutoff = max_width - 1;
        let safe_cutoff = choice
            .char_indices()
            .take_while(|(idx, _)| *idx <= cutoff)
            .map(|(idx, _)| idx)
            .last()
            .unwrap_or(0);

        choice.truncate(safe_cutoff);
        choice.push_str(ELLIPSIS);
    }

    choice
}

pub fn get_youtube_id(link: &str) -> Option<String> {
    // Try to parse the URL; bail out if it's invalid
    tracing::debug!("Parsing YouTube link {}", link);
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


pub async fn get_vc_id(ctx: PoiseContext<'_>) -> Result<ChannelId, Error> {
    let guild_id = require_guild(ctx)?;

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

pub fn require_guild(ctx: PoiseContext<'_>) -> Result<GuildId, Error> {
    ctx.guild_id().ok_or_else(|| "This command can only be used in a server.".into())
}

pub async fn join_vc(ctx: PoiseContext<'_>, guild: Guild, vc_id: ChannelId) -> Result<Arc<Mutex<Call>>, Error>{
    tracing::debug!("Joining user's voice chat");

    let manager = songbird::get(ctx.serenity_context())
        .await
        .expect("Error getting the Songbird client from the manager")
        .clone();

    let join_result = manager.join(guild.id, vc_id).await;
    Ok(join_result?)
}