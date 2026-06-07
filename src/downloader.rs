use std::process::Command;
use serde_json::Value;
use sqlx::SqlitePool;

use crate::library::{get_youtube_id, process_ytdlp_json};
use crate::definitions::{Error, MetadataKind, TrackInfo, VideoId};
use crate::repository::{get_or_insert_metadata_id, insert_new_track, lookup_track};


pub async fn download_track(
    db_pool: &SqlitePool,
    yt_link: String,
    track_artist: Option<String>,
    track_origin: Option<String>,
    track_title: Option<String>,
) -> Result<TrackInfo, Error> {
    let video_id = VideoId::from(
        get_youtube_id(&yt_link)
            .ok_or("Invalid YouTube link")?
    );

    // Guard against duplicate downloads
    if let Some(track) = lookup_track(db_pool, &video_id).await? {
        return Ok(track);
    }

    let output = Command::new("./yt-dlp")
        .arg("-t")
        .arg("mp3")
        .arg("-o")
        .arg("audio/%(id)s.%(ext)s")
        .arg("--no-playlist")
        .arg("--write-info-json")
        .arg("--no-progress")
        .arg("--cookies")
        .arg("cookies.txt")
        .arg(&yt_link)
        .output()
        .map_err(|e| format!("Failed to execute yt-dlp: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "yt-dlp failed with error: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }

    let slim = process_ytdlp_json(video_id.as_str().to_string())
        .map_err(|e| {
            format!(
                "Failed to process metadata JSON for video ID `{}`: {}",
                video_id.as_str(),
                e
            )
        })?;

    let title = track_title.unwrap_or_else(|| {
        slim.get("title")
            .and_then(Value::as_str)
            .unwrap_or("Unknown Title")
            .to_string()
    });

    let artist = track_artist.unwrap_or_else(|| {
        "No artist provided".to_string()
    });

    let origin = track_origin.unwrap_or_else(|| {
        "No origin provided".to_string()
    });

    let artist_id =
        get_or_insert_metadata_id(db_pool, MetadataKind::Artist, &artist).await?;

    let origin_id =
        get_or_insert_metadata_id(db_pool, MetadataKind::Origin, &origin).await?;
    
    insert_new_track(db_pool, &video_id, &slim, &title, artist_id, origin_id).await?;

    Ok(TrackInfo {
        id: video_id,
        title,
        artist,
        origin,
    })
}