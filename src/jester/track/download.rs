use async_trait::async_trait;
use serde_json::Value;
use sqlx::SqlitePool;
use std::{
    path::PathBuf,
    process::{Command, Output},
    sync::Arc,
};

use crate::{
    discord::context::Error,
    jester::db::{
        metadata::MetadataKind,
        repository::{get_or_insert_metadata_id, insert_new_track, lookup_track},
    },
    jester::library::constants::{AUDIO_DIR, COOKIES_PATH, YTDLP_PATH},
    jester::track::{
        metadata::process_ytdlp_json_at,
        types::{TrackInfo, VideoId},
        youtube::get_youtube_id,
    },
};
use tracing::{debug, info, instrument, warn};

#[async_trait]
pub trait DownloadExecutor: Send + Sync {
    async fn output(&self, program: &str, args: &[String]) -> anyhow::Result<Output>;
}

struct ProcessExecutor;
#[async_trait]
impl DownloadExecutor for ProcessExecutor {
    async fn output(&self, program: &str, args: &[String]) -> anyhow::Result<Output> {
        Command::new(program)
            .args(args)
            .output()
            .map_err(Into::into)
    }
}

#[derive(Clone, Debug)]
pub struct DownloadConfig {
    pub audio_dir: PathBuf,
    pub ytdlp_path: PathBuf,
    pub cookies_path: PathBuf,
}

impl Default for DownloadConfig {
    fn default() -> Self {
        Self {
            audio_dir: AUDIO_DIR.into(),
            ytdlp_path: YTDLP_PATH.into(),
            cookies_path: COOKIES_PATH.into(),
        }
    }
}

#[instrument(skip(db_pool), fields(link = %yt_link))]
pub async fn download_track(
    db_pool: &SqlitePool,
    yt_link: String,
    track_artist: Option<String>,
    track_origin: Option<String>,
    track_title: Option<String>,
) -> Result<TrackInfo, Error> {
    download_track_with(
        db_pool,
        yt_link,
        track_artist,
        track_origin,
        track_title,
        Arc::new(ProcessExecutor),
        DownloadConfig::default(),
    )
    .await
}

pub async fn download_track_with(
    db_pool: &SqlitePool,
    yt_link: String,
    track_artist: Option<String>,
    track_origin: Option<String>,
    track_title: Option<String>,
    executor: Arc<dyn DownloadExecutor>,
    config: DownloadConfig,
) -> Result<TrackInfo, Error> {
    let video_id = VideoId::from(get_youtube_id(&yt_link).ok_or("Invalid YouTube link")?);
    info!(track_id = %video_id.as_str(), "Starting track download");

    // Guard against duplicate downloads
    if let Some(track) = lookup_track(db_pool, &video_id).await? {
        debug!(track_id = %video_id.as_str(), "Skipping duplicate track download");
        return Ok(track);
    }

    let args = vec![
        "-t".into(),
        "mp3".into(),
        "-o".into(),
        config
            .audio_dir
            .join("%(id)s.%(ext)s")
            .to_string_lossy()
            .into_owned(),
        "--no-playlist".into(),
        "--write-info-json".into(),
        "--no-progress".into(),
        "--cookies".into(),
        config.cookies_path.to_string_lossy().into_owned(),
        yt_link,
    ];
    let output = executor
        .output(&config.ytdlp_path.to_string_lossy(), &args)
        .await
        .map_err(|e| format!("Failed to execute yt-dlp: {e}"))?;

    if !output.status.success() {
        warn!(track_id = %video_id.as_str(), stderr = %String::from_utf8_lossy(&output.stderr), "yt-dlp returned non-zero exit");
        return Err(format!(
            "yt-dlp failed with error: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }

    let slim = process_ytdlp_json_at(&config.audio_dir, video_id.as_str()).map_err(|e| {
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

    let artist = track_artist.unwrap_or_else(|| "No artist provided".to_string());

    let origin = track_origin.unwrap_or_else(|| "No origin provided".to_string());

    let artist_id = get_or_insert_metadata_id(db_pool, MetadataKind::Artist, &artist).await?;

    let origin_id = get_or_insert_metadata_id(db_pool, MetadataKind::Origin, &origin).await?;

    insert_new_track(db_pool, &video_id, &slim, &title, artist_id, origin_id).await?;

    info!(track_id = %video_id.as_str(), %title, %artist, %origin, "Track downloaded and added to library");

    Ok(TrackInfo {
        id: video_id,
        title,
        artist,
        origin,
    })
}
