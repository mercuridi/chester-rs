use std::process::Command;
use serde_json::Value;
use sqlx::SqlitePool;

use crate::track_resolver::lookup_track;
use crate::library::{get_youtube_id, process_ytdlp_json, get_id_or_insert};
use crate::definitions::{Error, VideoId, TrackInfo};

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
        .expect("Failed to execute yt-dlp");

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
        get_id_or_insert(db_pool, "artist", &artist).await?;

    let origin_id =
        get_id_or_insert(db_pool, "origin", &origin).await?;

    sqlx::query(
        "INSERT INTO tracks (
            id,
            upload_date,
            yt_title,
            track_title,
            artist_id,
            origin_id
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )
    .bind(video_id.as_str())
    .bind(
        slim.get("upload_date")
            .and_then(Value::as_str)
            .unwrap_or("Unknown Date"),
    )
    .bind(
        slim.get("title")
            .and_then(Value::as_str)
            .unwrap_or("Unknown Title"),
    )
    .bind(&title)
    .bind(artist_id)
    .bind(origin_id)
    .execute(db_pool)
    .await?;

    Ok(TrackInfo {
        id: video_id,
        title,
        artist,
        origin,
    })
}