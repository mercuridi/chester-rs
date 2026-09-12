use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use serde_json::Value;
use sqlx::SqlitePool;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Output,
    sync::Arc,
    time::Duration,
};
use tokio::{
    process::Command,
    sync::{Mutex, Semaphore},
};
use tracing::{info, instrument, warn};

use crate::jester::db::repository::{insert_new_track_with_metadata, lookup_track};
use crate::jester::track::{
    metadata::{metadata_sidecar_path, process_ytdlp_json_at, read_metadata_sidecar},
    types::{TrackInfo, VideoId},
    youtube::get_youtube_id,
};

#[async_trait]
pub trait DownloadExecutor: Send + Sync {
    async fn output(&self, program: &str, args: &[String]) -> Result<Output>;
}

struct ProcessExecutor;
#[async_trait]
impl DownloadExecutor for ProcessExecutor {
    async fn output(&self, program: &str, args: &[String]) -> Result<Output> {
        Command::new(program)
            .args(args)
            .kill_on_drop(true)
            .output()
            .await
            .map_err(Into::into)
    }
}

#[derive(Clone, Debug)]
pub struct DownloadConfig {
    pub audio_dir: PathBuf,
    pub ytdlp_path: PathBuf,
    pub cookies_path: PathBuf,
    pub ffmpeg_path: PathBuf,
    pub deadline: Duration,
    pub retries: usize,
    pub concurrency: usize,
}

impl From<&crate::chronicle::config::AppPaths> for DownloadConfig {
    fn from(paths: &crate::chronicle::config::AppPaths) -> Self {
        Self {
            audio_dir: paths.audio_dir.clone(),
            ytdlp_path: paths.ytdlp_path.clone(),
            cookies_path: paths.cookies_path.clone(),
            ffmpeg_path: "ffmpeg".into(),
            deadline: Duration::from_mins(5),
            retries: 3,
            concurrency: 4,
        }
    }
}

#[derive(Clone, Debug)]
pub struct DownloadedArtifact {
    pub audio_path: PathBuf,
    pub metadata: Option<Value>,
}

#[derive(Clone)]
pub struct Downloader {
    config: DownloadConfig,
    executor: Arc<dyn DownloadExecutor>,
    permits: Arc<Semaphore>,
    flights: Arc<Mutex<HashMap<VideoId, Arc<Mutex<()>>>>>,
    completed_metadata: Arc<Mutex<HashMap<VideoId, Option<Value>>>>,
}

impl Downloader {
    pub fn new(config: DownloadConfig) -> Arc<Self> {
        Self::with_executor(config, Arc::new(ProcessExecutor))
    }
    pub fn with_executor(config: DownloadConfig, executor: Arc<dyn DownloadExecutor>) -> Arc<Self> {
        Arc::new(Self {
            permits: Arc::new(Semaphore::new(config.concurrency.max(1))),
            config,
            executor,
            flights: Arc::new(Mutex::new(HashMap::new())),
            completed_metadata: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub async fn verify_dependencies(&self) -> Result<()> {
        let ytdlp = self
            .executor
            .output(
                &self.config.ytdlp_path.to_string_lossy(),
                &["--version".into()],
            )
            .await?;
        if !ytdlp.status.success() {
            anyhow::bail!("yt-dlp version check returned a non-zero exit status");
        }
        let ffmpeg = self
            .executor
            .output(
                &self.config.ffmpeg_path.to_string_lossy(),
                &["-version".into()],
            )
            .await?;
        if !ffmpeg.status.success() {
            anyhow::bail!("ffmpeg version check returned a non-zero exit status");
        }
        Ok(())
    }

    pub fn audio_path(&self, id: &str) -> PathBuf {
        self.config.audio_dir.join(format!("{id}.mp3"))
    }

    async fn id_lock(&self, id: &VideoId) -> Arc<Mutex<()>> {
        let mut flights = self.flights.lock().await;
        flights
            .entry(id.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    pub async fn download(
        &self,
        id: VideoId,
        source: String,
        include_metadata: bool,
    ) -> Result<DownloadedArtifact> {
        let lock = self.id_lock(&id).await;
        let _id_guard = lock.lock().await;
        self.download_unlocked(id, source, include_metadata).await
    }

    async fn download_unlocked(
        &self,
        id: VideoId,
        source: String,
        include_metadata: bool,
    ) -> Result<DownloadedArtifact> {
        let final_path = self.config.audio_dir.join(format!("{}.mp3", id.as_str()));
        if tokio::fs::try_exists(&final_path).await.unwrap_or(false) {
            let cached_metadata = self
                .completed_metadata
                .lock()
                .await
                .get(&id)
                .cloned()
                .flatten();
            let metadata = if !include_metadata {
                cached_metadata
            } else if cached_metadata.is_some() {
                cached_metadata
            } else if tokio::fs::try_exists(metadata_sidecar_path(
                &self.config.audio_dir,
                id.as_str(),
            ))
            .await
            .unwrap_or(false)
            {
                let metadata = read_metadata_sidecar(&self.config.audio_dir, id.as_str())?;
                self.completed_metadata
                    .lock()
                    .await
                    .insert(id.clone(), Some(metadata.clone()));
                Some(metadata)
            } else {
                let metadata = self.fetch_metadata(&id, &source).await?;
                self.persist_metadata(&id, &metadata).await?;
                self.completed_metadata
                    .lock()
                    .await
                    .insert(id.clone(), Some(metadata.clone()));
                Some(metadata)
            };
            return Ok(DownloadedArtifact {
                audio_path: final_path,
                metadata,
            });
        }
        let _permit = self.permits.acquire().await.context("Downloader stopped")?;
        tokio::fs::create_dir_all(&self.config.audio_dir)
            .await
            .context("Failed to create audio directory")?;
        let staging = tempfile::tempdir_in(&self.config.audio_dir)
            .context("Failed to create download staging directory")?;
        let staged_base = staging.path().join(id.as_str());
        let deadline = tokio::time::Instant::now() + self.config.deadline;
        let (staged_audio, metadata) = self
            .download_attempts(&id, &source, &staged_base, include_metadata, deadline)
            .await?;
        if let Some(metadata) = &metadata {
            self.persist_metadata(&id, metadata).await?;
        }
        tokio::fs::rename(staged_audio, &final_path)
            .await
            .context("Failed to finalize downloaded audio")?;
        self.completed_metadata
            .lock()
            .await
            .insert(id.clone(), metadata.clone());
        Ok(DownloadedArtifact {
            audio_path: final_path,
            metadata,
        })
    }

    async fn persist_metadata(&self, id: &VideoId, metadata: &Value) -> Result<()> {
        let path = metadata_sidecar_path(&self.config.audio_dir, id.as_str());
        let temporary = tempfile::NamedTempFile::new_in(&self.config.audio_dir)
            .context("Failed to create temporary metadata sidecar")?;
        let temporary_path = temporary.path().to_path_buf();
        let content =
            serde_json::to_vec_pretty(metadata).context("Failed to serialize metadata")?;
        tokio::fs::write(&temporary_path, content)
            .await
            .with_context(|| {
                format!(
                    "Failed to write metadata sidecar {}",
                    temporary_path.display()
                )
            })?;
        tokio::fs::rename(&temporary_path, &path)
            .await
            .with_context(|| format!("Failed to finalize metadata sidecar {}", path.display()))?;
        Ok(())
    }

    async fn fetch_metadata(&self, id: &VideoId, source: &str) -> Result<Value> {
        let _permit = self.permits.acquire().await.context("Downloader stopped")?;
        tokio::fs::create_dir_all(&self.config.audio_dir)
            .await
            .context("Failed to create audio directory")?;
        let staging = tempfile::tempdir_in(&self.config.audio_dir)
            .context("Failed to create metadata staging directory")?;
        let staged_base = staging.path().join(id.as_str());
        let attempts = self.config.retries.max(1);
        let deadline = tokio::time::Instant::now() + self.config.deadline;

        for attempt in 1..=attempts {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            let args = vec![
                "--skip-download".into(),
                "--write-info-json".into(),
                "--no-playlist".into(),
                "--no-progress".into(),
                "-o".into(),
                format!("{}.%(ext)s", staged_base.display()),
                "--cookies".into(),
                self.config.cookies_path.to_string_lossy().into_owned(),
                source.into(),
            ];
            let output = match tokio::time::timeout(
                remaining,
                self.executor
                    .output(&self.config.ytdlp_path.to_string_lossy(), &args),
            )
            .await
            {
                Ok(Ok(output)) => output,
                Ok(Err(error)) => {
                    warn!(track_id = %id.as_str(), attempt, error = %error, "yt-dlp metadata process failed");
                    continue;
                }
                Err(_) => break,
            };
            if output.status.success() {
                return process_ytdlp_json_at(staging.path(), id.as_str())
                    .context("Failed to process recovered yt-dlp metadata");
            }
            warn!(track_id = %id.as_str(), attempt, stderr = %String::from_utf8_lossy(&output.stderr), "yt-dlp metadata fetch returned non-zero exit");
        }
        Err(anyhow!(
            "All yt-dlp metadata attempts failed for video ID {}",
            id.as_str()
        ))
    }

    async fn download_attempts(
        &self,
        id: &VideoId,
        source: &str,
        staged_base: &Path,
        include_metadata: bool,
        deadline: tokio::time::Instant,
    ) -> Result<(PathBuf, Option<Value>)> {
        let attempts = self.config.retries.max(1);
        for attempt in 1..=attempts {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            let output = match tokio::time::timeout(
                remaining,
                self.executor.output(
                    &self.config.ytdlp_path.to_string_lossy(),
                    &self.args(staged_base, source, include_metadata),
                ),
            )
            .await
            {
                Ok(Ok(output)) => output,
                Ok(Err(error)) => {
                    warn!(track_id = %id.as_str(), attempt, error = %error, "yt-dlp process failed");
                    if attempt < attempts {
                        tokio::time::sleep(
                            Duration::from_millis(200 * attempt as u64).min(
                                deadline.saturating_duration_since(tokio::time::Instant::now()),
                            ),
                        )
                        .await;
                    }
                    continue;
                }
                Err(_) => {
                    warn!(track_id = %id.as_str(), attempt, "yt-dlp download deadline exceeded");
                    break;
                }
            };
            if output.status.success() {
                let audio = staged_base.with_extension("mp3");
                if tokio::fs::try_exists(&audio).await.unwrap_or(false) {
                    let metadata = if include_metadata {
                        Some(
                            process_ytdlp_json_at(
                                staged_base.parent().unwrap_or(Path::new(".")),
                                id.as_str(),
                            )
                            .context("Failed to process yt-dlp metadata")?,
                        )
                    } else {
                        None
                    };
                    return Ok((audio, metadata));
                }
            } else {
                warn!(track_id = %id.as_str(), attempt, stderr = %String::from_utf8_lossy(&output.stderr), "yt-dlp returned non-zero exit");
            }
            if attempt < attempts {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                tokio::time::sleep(Duration::from_millis(200 * attempt as u64).min(remaining))
                    .await;
            }
        }
        Err(anyhow!(
            "All yt-dlp download attempts failed for video ID {}",
            id.as_str()
        ))
    }

    fn args(&self, staged_base: &Path, source: &str, include_metadata: bool) -> Vec<String> {
        let mut args = vec![
            "-x".into(),
            "--audio-format".into(),
            "mp3".into(),
            "--audio-quality".into(),
            "0".into(),
            "--no-playlist".into(),
            "--no-progress".into(),
            "-o".into(),
            format!("{}.%(ext)s", staged_base.display()),
            "--cookies".into(),
            self.config.cookies_path.to_string_lossy().into_owned(),
        ];
        if include_metadata {
            args.push("--write-info-json".into());
        }
        args.push(source.into());
        args
    }
}

#[instrument(skip(db_pool, downloader), fields(link = %yt_link))]
pub async fn download_track(
    db_pool: &SqlitePool,
    yt_link: String,
    track_artist: Option<String>,
    track_origin: Option<String>,
    track_title: Option<String>,
    downloader: Arc<Downloader>,
) -> Result<TrackInfo> {
    let video_id =
        VideoId::from(get_youtube_id(&yt_link).ok_or_else(|| anyhow!("Invalid YouTube link"))?);

    let lock = downloader.id_lock(&video_id).await;
    let _id_guard = lock.lock().await;

    if let Some(track) = lookup_track(db_pool, &video_id).await? {
        return Ok(track);
    }
    let artifact = downloader
        .download_unlocked(video_id.clone(), yt_link, true)
        .await?;
    let metadata = artifact
        .metadata
        .as_ref()
        .context("Downloader did not return metadata")?;
    let title = track_title.unwrap_or_else(|| {
        metadata
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("Unknown Title")
            .to_string()
    });
    if let Err(error) = insert_new_track_with_metadata(
        db_pool,
        &video_id,
        metadata,
        &title,
        track_artist.as_deref(),
        track_origin.as_deref(),
    )
    .await
    {
        // A database-level race may still occur across Downloader instances or
        // processes. If another caller completed registration, this operation
        // is successful from the caller's perspective and the finalized audio
        // belongs to the registered track, so it must not be removed.
        if let Some(track) = lookup_track(db_pool, &video_id).await? {
            return Ok(track);
        }
        let _ = tokio::fs::remove_file(&artifact.audio_path).await;
        let _ = tokio::fs::remove_file(metadata_sidecar_path(
            &downloader.config.audio_dir,
            video_id.as_str(),
        ))
        .await;
        return Err(error);
    }
    info!(track_id = %video_id.as_str(), %title, "Track downloaded and added to library");
    Ok(TrackInfo {
        id: video_id,
        title,
        artist: track_artist,
        origin: track_origin,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::process::ExitStatusExt, sync::Mutex};
    use tempfile::tempdir;

    struct FakeExecutor {
        calls: Mutex<usize>,
    }
    #[async_trait]
    impl DownloadExecutor for FakeExecutor {
        async fn output(&self, _program: &str, args: &[String]) -> Result<Output> {
            *self
                .calls
                .lock()
                .map_err(|_| anyhow!("fake executor call counter poisoned"))? += 1;
            let path = args
                .iter()
                .find(|arg| arg.contains("%(ext)s"))
                .ok_or_else(|| anyhow!("download output path argument missing"))?
                .replace("%(ext)s", "mp3");
            std::fs::write(path, b"audio")?;
            if args.iter().any(|arg| arg == "--write-info-json") {
                let base = args
                    .iter()
                    .find(|arg| arg.contains("%(ext)s"))
                    .expect("output path was found above")
                    .replace(".%(ext)s", "");
                std::fs::write(
                    format!("{base}.info.json"),
                    r#"{"id":"abc","upload_date":"20260101","title":"Downloaded","channel":"Test Artist"}"#,
                )?;
            }
            Ok(Output {
                status: std::process::ExitStatus::from_raw(0),
                stdout: vec![],
                stderr: vec![],
            })
        }
    }

    fn config(dir: &Path) -> DownloadConfig {
        DownloadConfig {
            audio_dir: dir.into(),
            ytdlp_path: "yt-dlp".into(),
            cookies_path: "cookies".into(),
            ffmpeg_path: "ffmpeg".into(),
            deadline: Duration::from_secs(5),
            retries: 2,
            concurrency: 2,
        }
    }

    #[tokio::test]
    async fn concurrent_calls_for_one_id_execute_once() -> Result<()> {
        let dir = tempdir()?;
        let executor = Arc::new(FakeExecutor {
            calls: Mutex::new(0),
        });
        let downloader = Downloader::with_executor(config(dir.path()), executor.clone());
        let (first, second) = tokio::join!(
            downloader.download(
                VideoId::from("abc"),
                "https://youtube.test/abc".into(),
                false
            ),
            downloader.download(
                VideoId::from("abc"),
                "https://youtube.test/abc".into(),
                false
            ),
        );
        assert_eq!(first?.audio_path, second?.audio_path);
        assert_eq!(
            *executor
                .calls
                .lock()
                .map_err(|_| anyhow!("fake executor call counter poisoned"))?,
            1
        );
        assert!(dir.path().join("abc.mp3").exists());
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_track_registration_is_idempotent_and_preserves_audio() -> Result<()> {
        let dir = tempdir()?;
        let database_url = format!("sqlite://{}", dir.path().join("jester.db").display());
        let pool = crate::database::pool::open_sqlite_pool(&database_url, "test").await?;
        crate::jester::db::schema::initialise(&pool).await?;

        let executor = Arc::new(FakeExecutor {
            calls: Mutex::new(0),
        });
        let downloader = Downloader::with_executor(config(dir.path()), executor.clone());
        let (first, second) = tokio::join!(
            download_track(
                &pool,
                "https://www.youtube.com/watch?v=abc".into(),
                None,
                None,
                None,
                downloader.clone(),
            ),
            download_track(
                &pool,
                "https://www.youtube.com/watch?v=abc".into(),
                None,
                None,
                None,
                downloader,
            ),
        );

        assert_eq!(first?.id, VideoId::from("abc"));
        assert_eq!(second?.id, VideoId::from("abc"));
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM tracks WHERE id = 'abc'")
                .fetch_one(&pool)
                .await?,
            1
        );
        assert!(dir.path().join("abc.mp3").exists());
        assert_eq!(
            *executor
                .calls
                .lock()
                .map_err(|_| anyhow!("fake executor call counter poisoned"))?,
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn existing_audio_can_be_registered_by_a_fresh_downloader() -> Result<()> {
        let dir = tempdir()?;
        let database_url = format!("sqlite://{}", dir.path().join("jester.db").display());
        let pool = crate::database::pool::open_sqlite_pool(&database_url, "test").await?;
        crate::jester::db::schema::initialise(&pool).await?;
        let executor = Arc::new(FakeExecutor {
            calls: Mutex::new(0),
        });
        let first = Downloader::with_executor(config(dir.path()), executor.clone());
        first
            .download(
                VideoId::from("abc"),
                "https://youtube.test/abc".into(),
                true,
            )
            .await?;

        let second = Downloader::with_executor(config(dir.path()), executor.clone());
        let track = download_track(
            &pool,
            "https://www.youtube.com/watch?v=abc".into(),
            None,
            None,
            None,
            second,
        )
        .await?;
        assert_eq!(track.title, "Downloaded");
        assert!(metadata_sidecar_path(dir.path(), "abc").exists());
        assert_eq!(
            *executor
                .calls
                .lock()
                .map_err(|_| anyhow!("counter poisoned"))?,
            1
        );
        Ok(())
    }
}
