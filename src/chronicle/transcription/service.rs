use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use serenity::model::id::UserId;

use super::{audio::load_opus, whisper::transcriber::WhisperTranscriber};
use crate::chronicle::runtime::GpuRuntime;

pub struct TranscribedSegment {
    pub start: f64,
    pub end: f64,
    pub user_id: UserId,
    pub text: String,
}

#[derive(Clone)]
pub struct TranscriptionService {
    runtime: GpuRuntime,
}

impl TranscriptionService {
    pub fn new(runtime: GpuRuntime) -> Self {
        Self { runtime }
    }

    /// Transcribe a set of per-user Opus recordings with exclusive GPU access.
    pub async fn transcribe_recordings(
        &self,
        recordings: Vec<PathBuf>,
    ) -> Result<Vec<TranscribedSegment>> {
        let _gpu_lease = self.runtime.acquire_transcription()?;

        tokio::task::spawn_blocking(move || {
            let mut transcriber = WhisperTranscriber::new_cuda()?;
            let mut output = Vec::new();

            for path in recordings {
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

            Ok::<_, anyhow::Error>(output)
        })
        .await
        .context("Transcription task failed")?
    }
}
