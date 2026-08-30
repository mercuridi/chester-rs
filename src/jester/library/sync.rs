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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{
        CommandExecutor, DownloadResult, SyncConfig, download_track, download_with_retry,
        process_track, verify_dependencies,
    };
    use anyhow::{Result, anyhow};
    use std::{
        collections::VecDeque,
        fs,
        os::unix::process::ExitStatusExt,
        process::{ExitStatus, Output},
        sync::Mutex,
    };
    use tempfile::tempdir;

    struct Response {
        success: bool,
        create_output_file: bool,
        error: Option<&'static str>,
    }

    struct FakeExecutor {
        responses: Mutex<VecDeque<Response>>,
        calls: Mutex<Vec<(String, Vec<String>)>>,
    }

    impl FakeExecutor {
        fn new(responses: impl IntoIterator<Item = Response>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().collect()),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn call_count(&self) -> Result<usize> {
            self.calls
                .lock()
                .map(|calls| calls.len())
                .map_err(|_| anyhow!("calls poisoned"))
        }
    }

    #[async_trait::async_trait]
    impl CommandExecutor for FakeExecutor {
        async fn output(&self, program: &str, args: &[String]) -> Result<Output> {
            self.calls
                .lock()
                .map_err(|_| anyhow!("calls poisoned"))?
                .push((program.into(), args.to_vec()));
            let response = self
                .responses
                .lock()
                .map_err(|_| anyhow!("responses poisoned"))?
                .pop_front()
                .ok_or_else(|| anyhow!("no fake response"))?;
            if let Some(error) = response.error {
                return Err(anyhow!(error));
            }
            if response.create_output_file
                && let Some(index) = args.iter().position(|arg| arg == "-o")
                && let Some(path) = args.get(index + 1)
            {
                fs::write(path, b"audio")?;
            }
            Ok(Output {
                status: ExitStatus::from_raw(if response.success { 0 } else { 1 << 8 }),
                stdout: Vec::new(),
                stderr: b"failure".to_vec(),
            })
        }
    }

    fn success(create_output_file: bool) -> Response {
        Response {
            success: true,
            create_output_file,
            error: None,
        }
    }

    fn failure() -> Response {
        Response {
            success: false,
            create_output_file: false,
            error: None,
        }
    }

    fn config(path: &std::path::Path) -> SyncConfig {
        SyncConfig {
            audio_dir: path.into(),
            ytdlp_path: "test-yt-dlp".into(),
            ffmpeg_path: "test-ffmpeg".into(),
            retries: 2,
        }
    }

    #[tokio::test]
    async fn dependency_check_invokes_expected_version_commands() -> Result<()> {
        let directory = tempdir()?;
        let executor = FakeExecutor::new([success(false), success(false)]);
        verify_dependencies(&executor, &config(directory.path())).await?;
        let calls = executor
            .calls
            .lock()
            .map_err(|_| anyhow!("calls poisoned"))?;
        assert_eq!(calls[0], ("test-yt-dlp".into(), vec!["--version".into()]));
        assert_eq!(calls[1], ("test-ffmpeg".into(), vec!["-version".into()]));
        Ok(())
    }

    #[tokio::test]
    async fn dependency_check_stops_after_ytdlp_failure() -> Result<()> {
        let directory = tempdir()?;
        let executor = FakeExecutor::new([failure()]);
        let error = verify_dependencies(&executor, &config(directory.path()))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("yt-dlp version check"));
        assert_eq!(executor.call_count()?, 1);
        Ok(())
    }

    #[tokio::test]
    async fn dependency_check_reports_ffmpeg_failure() -> Result<()> {
        let directory = tempdir()?;
        let executor = FakeExecutor::new([success(false), failure()]);
        let error = verify_dependencies(&executor, &config(directory.path()))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("ffmpeg version check"));
        assert_eq!(executor.call_count()?, 2);
        Ok(())
    }

    #[tokio::test]
    async fn download_builds_expected_command_and_finalises_file() -> Result<()> {
        let directory = tempdir()?;
        let executor = FakeExecutor::new([success(true)]);
        assert!(download_track("abc", &executor, &config(directory.path())).await?);
        assert_eq!(fs::read(directory.path().join("abc.mp3"))?, b"audio");
        assert!(!directory.path().join("abc.part.mp3").exists());
        let calls = executor
            .calls
            .lock()
            .map_err(|_| anyhow!("calls poisoned"))?;
        assert_eq!(calls[0].0, "test-yt-dlp");
        assert!(
            calls[0]
                .1
                .windows(2)
                .any(|pair| pair == ["--audio-format", "mp3"])
        );
        assert_eq!(
            calls[0].1.last().map(String::as_str),
            Some("https://www.youtube.com/watch?v=abc")
        );
        Ok(())
    }

    #[tokio::test]
    async fn successful_process_without_output_file_is_an_error() -> Result<()> {
        let directory = tempdir()?;
        let executor = FakeExecutor::new([success(false)]);
        let error = download_track("abc", &executor, &config(directory.path()))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("Downloaded file not found"));
        Ok(())
    }

    #[tokio::test]
    async fn existing_track_is_not_downloaded() -> Result<()> {
        let directory = tempdir()?;
        fs::write(directory.path().join("abc.mp3"), b"existing")?;
        let executor = std::sync::Arc::new(FakeExecutor::new([]));
        let result = process_track("abc", executor.clone(), config(directory.path())).await;
        assert!(matches!(result, DownloadResult::AlreadyPresent));
        assert_eq!(executor.call_count()?, 0);
        Ok(())
    }

    #[tokio::test]
    async fn retries_failed_download_and_then_succeeds() -> Result<()> {
        let directory = tempdir()?;
        let executor = std::sync::Arc::new(FakeExecutor::new([failure(), success(true)]));
        assert!(download_with_retry("abc", executor.clone(), config(directory.path())).await?);
        assert_eq!(executor.call_count()?, 2);
        Ok(())
    }

    #[tokio::test]
    async fn zero_retries_fails_without_invoking_executor() -> Result<()> {
        let directory = tempdir()?;
        let executor = std::sync::Arc::new(FakeExecutor::new([]));
        let mut value = config(directory.path());
        value.retries = 0;
        assert!(
            download_with_retry("abc", executor.clone(), value)
                .await
                .is_err()
        );
        assert_eq!(executor.call_count()?, 0);
        Ok(())
    }
}
