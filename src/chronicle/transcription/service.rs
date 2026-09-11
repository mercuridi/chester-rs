use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use serenity::model::id::UserId;

use super::{audio::load_opus, whisper::transcriber::WhisperTranscriber};
use crate::chronicle::runtime::{GpuRuntime, report_cuda_oom};
use tracing::{debug, info, instrument};

fn user_id_from_recording_path(path: &std::path::Path) -> Result<UserId> {
    path.file_stem()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("recording-"))
        .ok_or_else(|| anyhow!("Invalid recording filename: {}", path.display()))?
        .parse::<u64>()
        .map(UserId::new)
        .map_err(|error| {
            anyhow!(
                "Invalid user ID in recording filename {}: {error}",
                path.display()
            )
        })
}

pub trait Transcriber: Send {
    fn transcribe(
        &mut self,
        audio: &super::audio::Audio,
    ) -> Result<Vec<super::whisper::transcriber::TranscriptSegment>>;
}

pub trait TranscriberFactory: Send + Sync {
    fn create(&self) -> Result<Box<dyn Transcriber>>;
}

struct CudaTranscriberFactory;
impl TranscriberFactory for CudaTranscriberFactory {
    fn create(&self) -> Result<Box<dyn Transcriber>> {
        Ok(Box::new(WhisperTranscriber::new_cuda()?))
    }
}

pub struct TranscribedSegment {
    pub start: f64,
    pub end: f64,
    pub user_id: UserId,
    pub text: String,
}

#[derive(Clone)]
pub struct TranscriptionService {
    runtime: GpuRuntime,
    factory: Arc<dyn TranscriberFactory>,
}

impl TranscriptionService {
    pub fn new(runtime: GpuRuntime) -> Self {
        Self {
            runtime,
            factory: Arc::new(CudaTranscriberFactory),
        }
    }

    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "test dependency injection seam")
    )]
    pub fn with_factory(runtime: GpuRuntime, factory: Arc<dyn TranscriberFactory>) -> Self {
        Self { runtime, factory }
    }

    /// Transcribe a set of per-user Opus recordings with exclusive GPU access.
    #[instrument(skip(self, recordings), fields(recording_count = recordings.len()))]
    pub async fn transcribe_recordings(
        &self,
        recordings: Vec<PathBuf>,
    ) -> Result<Vec<TranscribedSegment>> {
        let gpu_lease = self.runtime.acquire_transcription()?;

        info!("Starting recording transcription");
        let factory = Arc::clone(&self.factory);
        let result = tokio::task::spawn_blocking(move || {
            // The blocking worker, rather than the async caller, owns the GPU
            // lease. Aborting the caller must not release the lease while this
            // work is still using the GPU.
            let _gpu_lease = gpu_lease;
            let mut transcriber = factory.create()?;
            let mut output = Vec::new();

            for path in recordings {
                debug!(path = %path.display(), "Transcribing recording");
                let audio = load_opus(&path)?;
                let segments = transcriber.transcribe(&audio)?;

                let user_id = user_id_from_recording_path(&path)?;

                output.extend(segments.into_iter().map(|segment| TranscribedSegment {
                    start: segment.start,
                    end: segment.end,
                    user_id,
                    text: segment.text,
                }));
            }

            output.sort_by(|a, b| {
                a.start
                    .partial_cmp(&b.start)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

            info!(
                segment_count = output.len(),
                "Recording transcription complete"
            );
            Ok::<_, anyhow::Error>(output)
        })
        .await
        .context("Transcription task failed")?;

        if let Err(error) = &result {
            report_cuda_oom(error, "transcription", "transcribe");
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::{TranscriberFactory, TranscriptionService, user_id_from_recording_path};
    use crate::chronicle::runtime::GpuRuntime;
    use anyhow::Result;
    use serenity::model::id::UserId;
    use std::{
        path::Path,
        sync::mpsc::{Receiver, Sender},
        sync::{Arc, Mutex},
        time::Duration,
    };

    struct NoopTranscriber;

    impl super::Transcriber for NoopTranscriber {
        fn transcribe(
            &mut self,
            _audio: &super::super::audio::Audio,
        ) -> Result<Vec<super::super::whisper::transcriber::TranscriptSegment>> {
            Ok(Vec::new())
        }
    }

    struct BlockingFactory {
        started: Sender<()>,
        release: Mutex<Option<Receiver<()>>>,
    }

    impl TranscriberFactory for BlockingFactory {
        fn create(&self) -> Result<Box<dyn super::Transcriber>> {
            self.started
                .send(())
                .map_err(|_| anyhow::anyhow!("test worker start receiver dropped"))?;
            let release = self
                .release
                .lock()
                .map_err(|_| anyhow::anyhow!("test release receiver poisoned"))?
                .take()
                .ok_or_else(|| anyhow::anyhow!("test worker release receiver missing"))?;
            release
                .recv()
                .map_err(|_| anyhow::anyhow!("test worker release sender dropped"))?;
            Ok(Box::new(NoopTranscriber))
        }
    }

    struct FailingFactory;

    impl TranscriberFactory for FailingFactory {
        fn create(&self) -> Result<Box<dyn super::Transcriber>> {
            anyhow::bail!("factory failed")
        }
    }

    #[test]
    fn extracts_user_id_from_recording_filename() -> Result<()> {
        assert_eq!(
            user_id_from_recording_path(Path::new("/tmp/recording-42.opus"))?,
            UserId::new(42)
        );
        Ok(())
    }

    #[test]
    fn rejects_invalid_recording_filenames() {
        for path in ["track-42.opus", "recording-.opus", "recording-user.opus"] {
            assert!(
                user_id_from_recording_path(Path::new(path)).is_err(),
                "{path}"
            );
        }
    }

    #[tokio::test]
    async fn factory_failure_is_reported_and_gpu_lease_is_released() -> Result<()> {
        let runtime = GpuRuntime::new();
        let service = TranscriptionService::with_factory(runtime.clone(), Arc::new(FailingFactory));
        let error = service
            .transcribe_recordings(Vec::new())
            .await
            .err()
            .ok_or_else(|| anyhow::anyhow!("factory failure should be returned"))?;
        assert!(error.to_string().contains("factory failed"));
        assert!(runtime.acquire_transcription().is_ok());
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelling_transcription_keeps_gpu_lease_until_worker_finishes() -> Result<()> {
        let runtime = GpuRuntime::new();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let factory = Arc::new(BlockingFactory {
            started: started_tx,
            release: Mutex::new(Some(release_rx)),
        });
        let service = TranscriptionService::with_factory(runtime.clone(), factory);

        let task = tokio::spawn(async move { service.transcribe_recordings(Vec::new()).await });
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .map_err(|_| anyhow::anyhow!("blocking transcription worker did not start"))?;

        task.abort();
        let _ = task.await;
        assert!(runtime.acquire_transcription().is_err());

        release_tx
            .send(())
            .map_err(|_| anyhow::anyhow!("blocking transcription worker already stopped"))?;

        for _ in 0..100 {
            if let Ok(lease) = runtime.acquire_transcription() {
                drop(lease);
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        anyhow::bail!("transcription GPU lease was not released after worker completion");
    }

    #[tokio::test]
    async fn transcription_is_rejected_while_llm_is_loaded() -> Result<()> {
        let runtime = GpuRuntime::new();
        runtime.begin_llm_load()?.commit_to_loaded()?;
        let service = TranscriptionService::with_factory(runtime, Arc::new(FailingFactory));
        let error = service
            .transcribe_recordings(Vec::new())
            .await
            .err()
            .ok_or_else(|| anyhow::anyhow!("busy runtime should reject transcription"))?;
        assert!(
            error
                .to_string()
                .contains("unavailable while the LLM is loaded")
        );
        Ok(())
    }
}
