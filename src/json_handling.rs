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