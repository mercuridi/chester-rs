use anyhow::{Context, Result, anyhow};
use candle_core::{Device, Tensor};
use candle_transformers::models::whisper::{self as m, audio};
use tokenizers::Tokenizer;

use crate::chronicle::transcription::audio::Audio;
use crate::chronicle::transcription::whisper::model::Model;
use crate::constants::{MODEL_SAMPLE_RATE, STRIDE_SIZE_SECONDS};

pub struct TranscriptSegment {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

pub struct WhisperTranscriber {
    pub model: Model,
    pub tokenizer: Tokenizer,
    pub device: Device,

    pub suppress_tokens: Tensor,

    pub sot_token: u32,
    pub language_token: u32,
    pub transcribe_token: u32,
    pub eot_token: u32,
    pub no_speech_token: u32,
    pub no_timestamps_token: u32,
}

impl WhisperTranscriber {
    /// Load Whisper Small English onto CUDA device 0.
    ///
    /// The first invocation downloads the model through hf-hub.
    /// Subsequent invocations use the local Hugging Face cache.
    pub fn new_cuda() -> Result<Self> {
        let device = Device::new_cuda(0).context("Failed to initialize CUDA device 0")?;

        Self::load(device)
    }

    /// Transcribe already-decoded 16 kHz mono audio.
    pub fn transcribe(&mut self, audio: &Audio) -> Result<Vec<TranscriptSegment>> {
        if audio.sample_rate != MODEL_SAMPLE_RATE {
            return Err(anyhow!(
                "Whisper expects {} Hz audio, got {} Hz",
                MODEL_SAMPLE_RATE,
                audio.sample_rate
            ));
        }

        if audio.samples.is_empty() {
            return Ok(Vec::new());
        }

        let mel_filters = load_mel_filters()?;

        let mel = audio::pcm_to_mel(self.model.config(), &audio.samples, &mel_filters);

        let mel_len = mel.len();

        let mel = Tensor::from_vec(
            mel,
            (
                1,
                self.model.config().num_mel_bins,
                mel_len / self.model.config().num_mel_bins,
            ),
            &self.device,
        )?;

        self.decode_mel(&mel)
    }

    fn decode_mel(&mut self, mel: &Tensor) -> Result<Vec<TranscriptSegment>> {
        let stride_frames =
            (STRIDE_SIZE_SECONDS * m::SAMPLE_RATE as f64 / m::HOP_LENGTH as f64) as usize;

        let (_, _, content_frames) = mel.dims3()?;

        let mut seek = 0;
        let mut segments = Vec::new();

        while seek < content_frames {
            let segment_size = usize::min(content_frames - seek, m::N_FRAMES);

            let mel_segment = self.pad_mel_segment(mel, segment_size, seek)?;

            let segment_start = (seek * m::HOP_LENGTH) as f64 / m::SAMPLE_RATE as f64;

            let segment_duration = (segment_size * m::HOP_LENGTH) as f64 / m::SAMPLE_RATE as f64;

            let decoded = self.decode_segment(&mel_segment)?;

            if decoded.no_speech_prob > m::NO_SPEECH_THRESHOLD
                && decoded.avg_logprob < m::LOGPROB_THRESHOLD
            {
                //tracing::info!("Whisper rejected segment");
                continue;
            }

            let timestamp_segments =
                self.extract_timestamp_segments(&decoded.tokens, segment_start, segment_duration)?;
            // tracing::info!(
            //     timestamp_segment_count = timestamp_segments.len(),
            //     "Whisper timestamp extraction"
            // );

            // for segment in &timestamp_segments {
            //     tracing::info!(
            //         start = segment.start,
            //         end = segment.end,
            //         text = %segment.text,
            //         "Whisper timestamp segment"
            //     );
            // }

            if timestamp_segments.is_empty() {
                let text = decoded.text.trim();

                if !text.is_empty() {
                    segments.push(TranscriptSegment {
                        start: segment_start,
                        end: segment_start + segment_duration,
                        text: text.to_owned(),
                    });
                }
            } else {
                segments.extend(timestamp_segments);
            }

            if segment_size == content_frames - seek {
                break;
            }

            seek += stride_frames;
        }

        let mut segments = deduplicate_segments(segments);
        segments.sort_by(|a, b| {
            a.start
                .total_cmp(&b.start)
                .then_with(|| a.end.total_cmp(&b.end))
        });

        Ok(segments)
    }

    pub fn is_timestamp_token(&self, token: u32) -> bool {
        token > self.no_timestamps_token
    }

    fn pad_mel_segment(&self, mel: &Tensor, segment_size: usize, seek: usize) -> Result<Tensor> {
        let mel_segment = mel.narrow(2, seek, segment_size)?;

        if segment_size >= m::N_FRAMES {
            return Ok(mel_segment);
        }

        let padding = Tensor::zeros(
            (
                1,
                self.model.config().num_mel_bins,
                m::N_FRAMES - segment_size,
            ),
            mel.dtype(),
            mel.device(),
        )?;

        Tensor::cat(&[&mel_segment, &padding], 2).map_err(Into::into)
    }
}

fn load_mel_filters() -> Result<Vec<f32>> {
    let bytes = include_bytes!("melfilters/melfilters128.bytes");

    if !bytes.len().is_multiple_of(4) {
        return Err(anyhow!("melfilters.bytes length is not divisible by 4"));
    }

    let mut filters = Vec::with_capacity(bytes.len() / 4);

    for chunk in bytes.chunks_exact(4) {
        filters.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }

    Ok(filters)
}

fn normalize_text(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

pub fn deduplicate_segments(mut segments: Vec<TranscriptSegment>) -> Vec<TranscriptSegment> {
    segments.sort_by(|a, b| {
        a.start
            .total_cmp(&b.start)
            .then_with(|| a.end.total_cmp(&b.end))
    });

    let mut output: Vec<TranscriptSegment> = Vec::new();

    for segment in segments {
        let Some(previous) = output.last_mut() else {
            output.push(segment);
            continue;
        };

        let overlaps = segment.start < previous.end;
        let same_text = normalize_text(&segment.text) == normalize_text(&previous.text);

        if overlaps && same_text {
            // Keep the shorter version.
            if (segment.end - segment.start) < (previous.end - previous.start) {
                *previous = segment;
            }

            continue;
        }

        output.push(segment);
    }

    output
}
