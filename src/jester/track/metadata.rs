use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::fs;

pub fn process_ytdlp_json(file_id: String) -> Result<serde_json::Value> {
    let path = format!("audio/{file_id}.info.json");
    let content =
        fs::read_to_string(&path).with_context(|| format!("Failed to read {:?}", path))?;

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
