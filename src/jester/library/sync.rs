use anyhow::{Context, Result};
use futures::stream::{self, StreamExt as FuturesStreamExt};
use sqlx::SqlitePool;
use std::sync::Arc;
use std::{path::PathBuf, time::Duration};
use tokio::process::Command;
use tracing::{debug, info, instrument, warn};

use crate::jester::library::constants::{AUDIO_DIR, DOWNLOAD_CONCURRENCY, MAX_RETRIES, YTDLP_PATH};

#[async_trait::async_trait]
pub trait CommandExecutor: Send + Sync {
    async fn output(&self, program: &str, args: &[String]) -> Result<std::process::Output>;
}

struct ProcessExecutor;
#[async_trait::async_trait]
impl CommandExecutor for ProcessExecutor {
    async fn output(&self, program: &str, args: &[String]) -> Result<std::process::Output> {
        Command::new(program)
            .args(args)
            .output()
            .await
            .map_err(Into::into)
    }
}

#[derive(Clone, Debug)]
pub struct SyncConfig {
    pub audio_dir: PathBuf,
    pub ytdlp_path: PathBuf,
    pub ffmpeg_path: PathBuf,
    pub retries: usize,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            audio_dir: PathBuf::from(AUDIO_DIR),
            ytdlp_path: PathBuf::from(YTDLP_PATH),
            ffmpeg_path: PathBuf::from("ffmpeg"),
            retries: MAX_RETRIES,
        }
    }
}

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
    sync_audio_library_with(pool, Arc::new(ProcessExecutor), SyncConfig::default()).await
}

pub async fn sync_audio_library_with(
    pool: &SqlitePool,
    executor: Arc<dyn CommandExecutor>,
    config: SyncConfig,
) -> Result<SyncStats> {
    info!("Starting audio library synchronization");

    verify_dependencies(executor.as_ref(), &config).await?;

    tokio::fs::create_dir_all(&config.audio_dir)
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
        .map(|id| {
            let executor = Arc::clone(&executor);
            let config = config.clone();
            async move {
                let result = process_track(&id, executor, config).await;
                (id, result)
            }
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

#[instrument(skip(executor, config))]
async fn verify_dependencies(executor: &dyn CommandExecutor, config: &SyncConfig) -> Result<()> {
    info!("Verifying yt-dlp and ffmpeg availability");

    let ytdlp = executor
        .output(
            config.ytdlp_path.to_str().unwrap_or(YTDLP_PATH),
            &["--version".into()],
        )
        .await
        .context("yt-dlp missing or not executable")?;
    if !ytdlp.status.success() {
        anyhow::bail!("yt-dlp version check returned a non-zero exit status");
    }

    let ffmpeg = executor
        .output(
            config.ffmpeg_path.to_str().unwrap_or("ffmpeg"),
            &["-version".into()],
        )
        .await
        .context("ffmpeg missing or not executable")?;
    if !ffmpeg.status.success() {
        anyhow::bail!("ffmpeg version check returned a non-zero exit status");
    }

    info!("yt-dlp and ffmpeg both available");

    Ok(())
}

async fn fetch_track_ids(pool: &SqlitePool) -> Result<Vec<String>> {
    sqlx::query_scalar::<_, String>("SELECT id FROM tracks")
        .fetch_all(pool)
        .await
        .context("Failed to fetch track IDs")
}

async fn process_track(
    id: &str,
    executor: Arc<dyn CommandExecutor>,
    config: SyncConfig,
) -> DownloadResult {
    let path = config.audio_dir.join(format!("{id}.mp3"));

    if tokio::fs::try_exists(&path).await.unwrap_or(false) {
        return DownloadResult::AlreadyPresent;
    }

    match download_with_retry(id, executor, config).await {
        Ok(true) => DownloadResult::Downloaded,
        Ok(false) => DownloadResult::Skipped,
        Err(_) => DownloadResult::Failed,
    }
}

#[instrument(skip(executor, config))]
async fn download_with_retry(
    id: &str,
    executor: Arc<dyn CommandExecutor>,
    config: SyncConfig,
) -> Result<bool> {
    for attempt in 1..=config.retries {
        match download_track(id, executor.as_ref(), &config).await {
            Ok(true) => return Ok(true),
            Ok(false) => return Ok(false),
            Err(e) => {
                warn!(
                    %id,
                    attempt,
                    error = %e,
                    "Download attempt failed"
                );

                if attempt < config.retries {
                    let backoff = Duration::from_millis(200 * attempt as u64);
                    tokio::time::sleep(backoff).await;
                }
            }
        }
    }

    anyhow::bail!("All yt-dlp download attempts failed")
}

#[instrument(skip(executor, config))]
async fn download_track(
    id: &str,
    executor: &dyn CommandExecutor,
    config: &SyncConfig,
) -> Result<bool> {
    let tmp_path = config.audio_dir.join(format!("{id}.part.mp3"));
    let final_path = config.audio_dir.join(format!("{id}.mp3"));

    let args = vec![
        "-x".into(),
        "--audio-format".into(),
        "mp3".into(),
        "--audio-quality".into(),
        "0".into(),
        "--no-playlist".into(),
        "--no-progress".into(),
        "-o".into(),
        tmp_path.to_string_lossy().into_owned(),
        format!("https://www.youtube.com/watch?v={id}"),
    ];
    let output = executor
        .output(config.ytdlp_path.to_str().unwrap_or(YTDLP_PATH), &args)
        .await
        .context("yt-dlp process failed")?;

    if !output.status.success() {
        warn!(
            %id,
            stderr = %String::from_utf8_lossy(&output.stderr),
            "yt-dlp returned non-zero exit"
        );
        anyhow::bail!("yt-dlp returned a non-zero exit status");
    }

    if tokio::fs::try_exists(&tmp_path).await.unwrap_or(false) {
        tokio::fs::rename(&tmp_path, &final_path)
            .await
            .context("Failed to finalize file")?;
        Ok(true)
    } else {
        warn!(%id, "Downloaded file not found after completion");
        anyhow::bail!("Downloaded file not found after yt-dlp completed");
    }
}
