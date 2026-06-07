use anyhow::{Context, Result};
use sqlx::SqlitePool;
use std::{path::PathBuf, time::Duration};
use tokio::process::Command;
use tracing::{debug, info, instrument, warn};
use futures::stream::{self, StreamExt as FuturesStreamExt};

const AUDIO_DIR: &str = "audio";
const DOWNLOAD_CONCURRENCY: usize = 4;
const MAX_RETRIES: usize = 3;
const YTDLP_PATH: &str = "./yt-dlp";

#[derive(Debug)]
pub struct SyncStats {
    pub total_tracks: usize,
    pub already_present: usize,
    pub downloaded: usize,
    pub failed: usize,
    pub skipped: usize,
}

#[derive(Debug)]
enum DownloadResult {
    AlreadyPresent,
    Downloaded,
    Failed,
    Skipped,
}

#[instrument(skip(pool))]
pub async fn sync_audio_library(pool: &SqlitePool) -> Result<SyncStats> {
    info!("Starting audio library synchronization");

    verify_dependencies().await?;

    tokio::fs::create_dir_all(AUDIO_DIR)
        .await
        .context("Failed to create audio directory")?;

    let ids = fetch_track_ids(pool).await?;
    let total_tracks = ids.len();

    let mut stats = SyncStats {
        total_tracks,
        already_present: 0,
        downloaded: 0,
        failed: 0,
        skipped: 0,
    };

    let mut tasks = stream::iter(ids)
        .map(|id| async move {
            let result = process_track(&id).await;
            (id, result)
        })
        .buffer_unordered(DOWNLOAD_CONCURRENCY);

    while let Some((id, result)) = tasks.next().await {
        match result {
            DownloadResult::AlreadyPresent => {
                stats.already_present += 1;
                debug!(%id, "Already present");
            }
            DownloadResult::Downloaded => {
                stats.downloaded += 1;
                debug!(%id, "Downloaded");
            }
            DownloadResult::Failed => {
                stats.failed += 1;
                warn!(%id, "Download failed");
            }
            DownloadResult::Skipped => {
                stats.skipped += 1;
                debug!(%id, "Skipped");
            }
        }
    }

    info!(
        total_tracks = stats.total_tracks,
        already_present = stats.already_present,
        downloaded = stats.downloaded,
        failed = stats.failed,
        skipped = stats.skipped,
        "Audio sync complete"
    );

    Ok(stats)
}

#[instrument]
async fn verify_dependencies() -> Result<()> {
    info!("Verifying yt-dlp and ffmpeg availability");

    Command::new(YTDLP_PATH)
        .arg("--version")
        .output()
        .await
        .context("yt-dlp missing or not executable")?;

    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .await
        .context("ffmpeg missing or not executable")?;

    Ok(())
}

async fn fetch_track_ids(pool: &SqlitePool) -> Result<Vec<String>> {
    sqlx::query_scalar::<_, String>("SELECT id FROM tracks")
        .fetch_all(pool)
        .await
        .context("Failed to fetch track IDs")
}

fn audio_path(id: &str) -> PathBuf {
    PathBuf::from(AUDIO_DIR).join(format!("{id}.mp3"))
}

async fn process_track(id: &str) -> DownloadResult {
    let path = audio_path(id);

    if tokio::fs::try_exists(&path).await.unwrap_or(false) {
        return DownloadResult::AlreadyPresent;
    }

    match download_with_retry(id).await {
        Ok(true) => DownloadResult::Downloaded,
        Ok(false) => DownloadResult::Skipped,
        Err(_) => DownloadResult::Failed,
    }
}

#[instrument]
async fn download_with_retry(id: &str) -> Result<bool> {
    for attempt in 1..=MAX_RETRIES {
        match download_track(id).await {
            Ok(true) => return Ok(true),
            Ok(false) => return Ok(false),
            Err(e) => {
                warn!(
                    %id,
                    attempt,
                    error = %e,
                    "Download attempt failed"
                );

                if attempt < MAX_RETRIES {
                    let backoff = Duration::from_millis(200 * attempt as u64);
                    tokio::time::sleep(backoff).await;
                }
            }
        }
    }

    Ok(false)
}

#[instrument]
async fn download_track(id: &str) -> Result<bool> {
    let tmp_path = format!("{AUDIO_DIR}/{id}.part.mp3");
    let final_path = audio_path(id);

    let output = Command::new(YTDLP_PATH)
        .arg("-x")
        .arg("--audio-format")
        .arg("mp3")
        .arg("--audio-quality")
        .arg("0")
        .arg("--no-playlist")
        .arg("--no-progress")
        .arg("-o")
        .arg(&tmp_path)
        .arg(format!("https://www.youtube.com/watch?v={id}"))
        .output()
        .await
        .context("yt-dlp process failed")?;

    if !output.status.success() {
        warn!(
            %id,
            stderr = %String::from_utf8_lossy(&output.stderr),
            "yt-dlp returned non-zero exit"
        );
        return Ok(false);
    }

    if tokio::fs::try_exists(&tmp_path).await.unwrap_or(false) {
        tokio::fs::rename(&tmp_path, &final_path)
            .await
            .context("Failed to finalize file")?;
        Ok(true)
    } else {
        warn!(%id, "Downloaded file not found after completion");
        Ok(false)
    }
}