use crate::jester::{
    library::constants::DOWNLOAD_CONCURRENCY,
    track::{download::Downloader, types::VideoId},
};
use anyhow::{Context, Result};
use futures::stream::{self, StreamExt};
use sqlx::SqlitePool;
use std::sync::Arc;

#[derive(Debug)]
pub struct SyncStats {
    pub total_tracks: usize,
    pub already_present: usize,
    pub downloaded: usize,
    pub failed: usize,
    pub skipped: usize,
}

#[derive(Clone)]
pub struct SyncConfig {
    pub downloader: Arc<Downloader>,
}

#[derive(Debug)]
enum DownloadResult {
    AlreadyPresent,
    Downloaded,
    Failed,
}

pub async fn sync_audio_library(pool: &SqlitePool, config: SyncConfig) -> Result<SyncStats> {
    config
        .downloader
        .verify_dependencies()
        .await
        .context("Failed to verify download dependencies")?;
    let ids = sqlx::query_scalar::<_, String>("SELECT id FROM tracks")
        .fetch_all(pool)
        .await
        .context("Failed to fetch track IDs")?;
    let total_tracks = ids.len();
    let downloader = config.downloader;
    let mut tasks = stream::iter(ids)
        .map(|id| {
            let downloader = downloader.clone();
            async move {
                let path_exists = tokio::fs::try_exists(downloader.audio_path(&id))
                    .await
                    .unwrap_or(false);
                if path_exists {
                    return DownloadResult::AlreadyPresent;
                }
                match downloader
                    .download(
                        VideoId::from(id.clone()),
                        format!("https://www.youtube.com/watch?v={id}"),
                        false,
                    )
                    .await
                {
                    Ok(_) => DownloadResult::Downloaded,
                    Err(_) => DownloadResult::Failed,
                }
            }
        })
        .buffer_unordered(DOWNLOAD_CONCURRENCY);
    let mut stats = SyncStats {
        total_tracks,
        already_present: 0,
        downloaded: 0,
        failed: 0,
        skipped: 0,
    };
    while let Some(result) = tasks.next().await {
        match result {
            DownloadResult::AlreadyPresent => stats.already_present += 1,
            DownloadResult::Downloaded => stats.downloaded += 1,
            DownloadResult::Failed => stats.failed += 1,
        }
    }
    Ok(stats)
}
