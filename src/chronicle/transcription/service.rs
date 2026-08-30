use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use serenity::model::id::UserId;

use super::{audio::load_opus, whisper::transcriber::WhisperTranscriber};
use crate::chronicle::runtime::GpuRuntime;
use tracing::{debug, info, instrument};

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

    pub fn with_factory(runtime: GpuRuntime, factory: Arc<dyn TranscriberFactory>) -> Self {
        Self { runtime, factory }
    }

    /// Transcribe a set of per-user Opus recordings with exclusive GPU access.
    #[instrument(skip(self, recordings), fields(recording_count = recordings.len()))]
    pub async fn transcribe_recordings(
        &self,
        recordings: Vec<PathBuf>,
    ) -> Result<Vec<TranscribedSegment>> {
        let _gpu_lease = self.runtime.acquire_transcription()?;

        info!("Starting recording transcription");
        let factory = Arc::clone(&self.factory);
        tokio::task::spawn_blocking(move || {
            let mut transcriber = factory.create()?;
            let mut output = Vec::new();

            for path in recordings {
                debug!(path = %path.display(), "Transcribing recording");
                let audio = load_opus(&path)?;
                let segments = transcriber.transcribe(&audio)?;

                let user_id = path
                    .file_stem()
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
                    })?;

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
        .context("Transcription task failed")?
    }
}
