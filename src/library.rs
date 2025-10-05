use crate::constants::{ELLIPSIS, ELLIPSIS_LEN};
use crate::definitions::{Context as DiscordContext, Error};

use songbird::Call;
use tokio::sync::Mutex;
use poise::serenity_prelude::{ChannelId, Guild};
use sqlx::{Sqlite, Pool};
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
        "id": v.get("id").cloned().unwrap(),
        "upload_date": v.get("upload_date").cloned().unwrap(),
        "title": v.get("title").cloned().unwrap(),
        "channel": v.get("channel").cloned().unwrap(),
    });

    fs::remove_file(&path).ok();

    Ok(slim)
}

// pub fn lightweight_trim(mut choice: String, max_width: usize) -> String {
//     if choice.len() > max_width - ELLIPSIS_LEN {
//         choice.truncate(max_width - ELLIPSIS_LEN);
//         choice.push_str(ELLIPSIS);
//     }
//     choice
// }

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

pub async fn get_id_or_insert(
    db_pool: &Pool<Sqlite>,
    field_name: &str, // singular, e.g. "tag"
    pls_find: &str,
) -> i64 {
    let table_name = format!("{}s", field_name);

    if &table_name == "tracks" {panic!("time for you to fix this type mismatch bug you left behind");}

    // Build SELECT statement with identifiers in the string
    let select_sql = format!("SELECT id FROM {} WHERE {} = ?1", table_name, field_name);

    match sqlx::query_scalar::<_, i64>(&select_sql)
        .bind(&pls_find)
        .fetch_optional(db_pool)
        .await
        .unwrap()
    {
        Some(id) => id,
        None => {
            // Insert new value
            let insert_sql = format!("INSERT INTO {} ({}) VALUES (?1)", table_name, field_name);
            sqlx::query(&insert_sql)
                .bind(&pls_find)
                .execute(db_pool)
                .await
                .unwrap();

            // Fetch its id
            sqlx::query_scalar::<_, i64>(&select_sql)
                .bind(&pls_find)
                .fetch_one(db_pool)
                .await
                .unwrap()
        }
    }
}

pub async fn get_vc_id(ctx: DiscordContext<'_>) -> Result<ChannelId, Error> {
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

pub async fn join_vc(ctx: DiscordContext<'_>, guild: Guild, vc_id: ChannelId) -> Result<Arc<Mutex<Call>>, Error>{
    println!("Joining user's voice chat");

    let manager = songbird::get(ctx.serenity_context())
        .await
        .expect("Error getting the Songbird client from the manager")
        .clone();

    let join_result = manager.join(guild.id, vc_id).await;
    Ok(join_result?)
}