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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::{os::unix::process::ExitStatusExt, sync::Mutex};
    use tempfile::tempdir;

    type TestResult<T = ()> = std::result::Result<T, crate::discord::context::Error>;

    struct FakeExecutor {
        success: bool,
        stderr: Vec<u8>,
        calls: Mutex<Vec<(String, Vec<String>)>>,
    }

    #[async_trait]
    impl DownloadExecutor for FakeExecutor {
        async fn output(&self, program: &str, args: &[String]) -> anyhow::Result<Output> {
            self.calls
                .lock()
                .unwrap()
                .push((program.into(), args.to_vec()));
            Ok(Output {
                status: std::process::ExitStatus::from_raw(if self.success { 0 } else { 1 << 8 }),
                stdout: Vec::new(),
                stderr: self.stderr.clone(),
            })
        }
    }

    async fn test_pool() -> TestResult<(tempfile::TempDir, SqlitePool)> {
        let directory = tempdir()?;
        let url = format!("sqlite://{}", directory.path().join("jester.db").display());
        let pool = crate::database::pool::open_sqlite_pool(&url, "test").await?;
        crate::jester::db::schema::initialise(&pool).await?;
        Ok((directory, pool))
    }

    fn config(audio_dir: &std::path::Path) -> DownloadConfig {
        DownloadConfig {
            audio_dir: audio_dir.into(),
            ytdlp_path: "test-yt-dlp".into(),
            cookies_path: "test-cookies.txt".into(),
        }
    }

    #[tokio::test]
    async fn rejects_invalid_links_without_executing_ytdlp() -> TestResult {
        let (_directory, pool) = test_pool().await?;
        let executor = Arc::new(FakeExecutor {
            success: true,
            stderr: Vec::new(),
            calls: Mutex::new(Vec::new()),
        });

        let error = download_track_with(
            &pool,
            "https://example.com/not-a-video".into(),
            None,
            None,
            None,
            executor.clone(),
            config(std::path::Path::new("/tmp")),
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("Invalid YouTube link"));
        assert!(executor.calls.lock().unwrap().is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn existing_track_skips_ytdlp_and_returns_persisted_metadata() -> TestResult {
        let (directory, pool) = test_pool().await?;
        let artist_id = get_or_insert_metadata_id(&pool, MetadataKind::Artist, "Artist").await?;
        let origin_id = get_or_insert_metadata_id(&pool, MetadataKind::Origin, "Origin").await?;
        let id = VideoId::from("dQw4w9WgXcQ");
        insert_new_track(
            &pool,
            &id,
            &serde_json::json!({"upload_date": "20260101", "title": "Source"}),
            "Saved title",
            artist_id,
            origin_id,
        )
        .await?;
        let executor = Arc::new(FakeExecutor {
            success: true,
            stderr: Vec::new(),
            calls: Mutex::new(Vec::new()),
        });

        let track = download_track_with(
            &pool,
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ".into(),
            None,
            None,
            None,
            executor.clone(),
            config(directory.path()),
        )
        .await?;

        assert_eq!(track.title, "Saved title");
        assert!(executor.calls.lock().unwrap().is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn ytdlp_failure_returns_stderr_and_preserves_database() -> TestResult {
        let (directory, pool) = test_pool().await?;
        let executor = Arc::new(FakeExecutor {
            success: false,
            stderr: b"network unavailable".to_vec(),
            calls: Mutex::new(Vec::new()),
        });

        let error = download_track_with(
            &pool,
            "https://youtu.be/dQw4w9WgXcQ".into(),
            None,
            None,
            None,
            executor.clone(),
            config(directory.path()),
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("network unavailable"));
        assert_eq!(executor.calls.lock().unwrap().len(), 1);
        assert!(
            lookup_track(&pool, &VideoId::from("dQw4w9WgXcQ"))
                .await?
                .is_none()
        );
        Ok(())
    }

    #[tokio::test]
    async fn successful_download_persists_metadata_and_uses_expected_arguments() -> TestResult {
        let (directory, pool) = test_pool().await?;
        std::fs::write(
            directory.path().join("dQw4w9WgXcQ.info.json"),
            r#"{"id":"dQw4w9WgXcQ","upload_date":"20260101","title":"Source title","channel":"Channel"}"#,
        )?;
        let executor = Arc::new(FakeExecutor {
            success: true,
            stderr: Vec::new(),
            calls: Mutex::new(Vec::new()),
        });

        let track = download_track_with(
            &pool,
            "https://youtu.be/dQw4w9WgXcQ".into(),
            Some("Artist".into()),
            Some("Origin".into()),
            None,
            executor.clone(),
            config(directory.path()),
        )
        .await?;

        assert_eq!(track.title, "Source title");
        assert_eq!(track.artist, "Artist");
        assert_eq!(track.origin, "Origin");
        assert!(!directory.path().join("dQw4w9WgXcQ.info.json").exists());
        {
            let calls = executor.calls.lock().unwrap();
            assert_eq!(calls[0].0, "test-yt-dlp");
            assert!(calls[0].1.windows(2).any(|pair| pair == ["-t", "mp3"]));
            assert!(
                calls[0]
                    .1
                    .windows(2)
                    .any(|pair| pair == ["--write-info-json", "--no-progress"])
            );
            assert_eq!(
                calls[0].1.last().map(String::as_str),
                Some("https://youtu.be/dQw4w9WgXcQ")
            );
        }
        assert_eq!(
            lookup_track(&pool, &VideoId::from("dQw4w9WgXcQ"))
                .await?
                .ok_or_else(|| std::io::Error::other("track should be persisted"))?
                .title,
            "Source title"
        );
        Ok(())
    }
}
