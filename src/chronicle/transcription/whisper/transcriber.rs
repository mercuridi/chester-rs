use std::collections::VecDeque;

use anyhow::{Context, Result, anyhow};
use candle_core::{Device, Tensor};
use candle_transformers::models::whisper::{self as m, audio};
use tokenizers::Tokenizer;
use tracing::{debug, info, warn};

use crate::chronicle::transcription::{AudioSource, MODEL_SAMPLE_RATE, Transcriber};

use super::model::Model;

pub struct TranscriptSegment {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

const ACTIVITY_FRAME_SAMPLES: usize = (MODEL_SAMPLE_RATE as usize) / 50;
const ACTIVITY_RMS_THRESHOLD: f32 = 0.001;
const SPEECH_PADDING_SAMPLES: usize = MODEL_SAMPLE_RATE as usize * 3 / 10;
const SILENCE_HANGOVER_SAMPLES: usize = MODEL_SAMPLE_RATE as usize / 2;
const MAX_REGION_SAMPLES: usize = 30 * MODEL_SAMPLE_RATE as usize;
const REGION_ADVANCE_SAMPLES: usize = 25 * MODEL_SAMPLE_RATE as usize;

struct SpeechRegion {
    start_sample: usize,
    samples: Vec<f32>,
}

#[derive(Default)]
struct SilenceSkipStats {
    section_count: usize,
    skipped_samples: usize,
}

impl SilenceSkipStats {
    fn record(&mut self, start_sample: usize, end_sample: usize) {
        if end_sample <= start_sample {
            return;
        }

        self.section_count += 1;
        self.skipped_samples += end_sample - start_sample;

        let samples_per_second = f64::from(MODEL_SAMPLE_RATE);
        debug!(
            start_seconds = start_sample as f64 / samples_per_second,
            end_seconds = end_sample as f64 / samples_per_second,
            duration_seconds = (end_sample - start_sample) as f64 / samples_per_second,
            "Skipping low-energy audio span before Whisper"
        );
    }
}

/// Streaming, low-energy speech gate. It retains only a short preroll and the
/// currently active region, so multi-hour recordings are never buffered whole.
struct SilenceScanner {
    pending_samples: Vec<f32>,
    preroll: VecDeque<f32>,
    active_samples: Vec<f32>,
    active_start_sample: usize,
    trailing_silence_samples: usize,
    processed_samples: usize,
}

impl SilenceScanner {
    fn new() -> Self {
        Self {
            pending_samples: Vec::with_capacity(ACTIVITY_FRAME_SAMPLES),
            preroll: VecDeque::with_capacity(SPEECH_PADDING_SAMPLES),
            active_samples: Vec::new(),
            active_start_sample: 0,
            trailing_silence_samples: 0,
            processed_samples: 0,
        }
    }

    fn push(&mut self, samples: &[f32]) -> Vec<SpeechRegion> {
        self.pending_samples.extend_from_slice(samples);
        let mut completed = Vec::new();
        let mut frame = Vec::with_capacity(ACTIVITY_FRAME_SAMPLES);
        let mut consumed_samples = 0usize;
        while self.pending_samples.len() - consumed_samples >= ACTIVITY_FRAME_SAMPLES {
            frame.clear();
            frame.extend_from_slice(
                &self.pending_samples[consumed_samples..consumed_samples + ACTIVITY_FRAME_SAMPLES],
            );
            consumed_samples += ACTIVITY_FRAME_SAMPLES;
            completed.extend(self.process_frame(&frame));
        }
        if consumed_samples > 0 {
            self.pending_samples.drain(..consumed_samples);
        }
        completed
    }

    fn finish(&mut self) -> Vec<SpeechRegion> {
        let mut completed = Vec::new();
        if !self.pending_samples.is_empty() {
            let frame = std::mem::take(&mut self.pending_samples);
            completed.extend(self.process_frame(&frame));
        }

        if !self.active_samples.is_empty() {
            self.trim_trailing_silence(SPEECH_PADDING_SAMPLES);
            completed.push(SpeechRegion {
                start_sample: self.active_start_sample,
                samples: std::mem::take(&mut self.active_samples),
            });
            self.trailing_silence_samples = 0;
        }

        completed
    }

    fn process_frame(&mut self, frame: &[f32]) -> Vec<SpeechRegion> {
        let frame_start_sample = self.processed_samples;
        self.processed_samples += frame.len();
        let mut completed = Vec::new();

        if frame_rms(frame) >= ACTIVITY_RMS_THRESHOLD {
            if self.active_samples.is_empty() {
                self.active_start_sample = frame_start_sample.saturating_sub(self.preroll.len());
                self.active_samples.extend(self.preroll.drain(..));
            }
            self.active_samples.extend_from_slice(frame);
            self.trailing_silence_samples = 0;
            self.preroll.clear();
        } else if self.active_samples.is_empty() {
            self.push_preroll(frame);
        } else {
            self.active_samples.extend_from_slice(frame);
            self.trailing_silence_samples += frame.len();
        }

        if !self.active_samples.is_empty() && self.active_samples.len() >= MAX_REGION_SAMPLES {
            let tail = self.active_samples.split_off(REGION_ADVANCE_SAMPLES);
            let mut head = std::mem::take(&mut self.active_samples);
            head.extend(
                tail.iter()
                    .take(MAX_REGION_SAMPLES - REGION_ADVANCE_SAMPLES),
            );
            self.active_samples = tail;
            completed.push(SpeechRegion {
                start_sample: self.active_start_sample,
                samples: head,
            });
            self.active_start_sample += REGION_ADVANCE_SAMPLES;
        }

        if !self.active_samples.is_empty()
            && self.trailing_silence_samples >= SILENCE_HANGOVER_SAMPLES
        {
            self.trim_trailing_silence(SPEECH_PADDING_SAMPLES);
            let region_samples = std::mem::take(&mut self.active_samples);
            self.preroll.extend(
                region_samples
                    .iter()
                    .rev()
                    .take(SPEECH_PADDING_SAMPLES)
                    .rev()
                    .copied(),
            );
            completed.push(SpeechRegion {
                start_sample: self.active_start_sample,
                samples: region_samples,
            });
            self.trailing_silence_samples = 0;
        }

        completed
    }

    fn push_preroll(&mut self, frame: &[f32]) {
        self.preroll.extend(frame.iter().copied());
        while self.preroll.len() > SPEECH_PADDING_SAMPLES {
            self.preroll.pop_front();
        }
    }

    fn trim_trailing_silence(&mut self, keep_samples: usize) {
        let discard_samples = self.trailing_silence_samples.saturating_sub(keep_samples);
        self.active_samples
            .truncate(self.active_samples.len().saturating_sub(discard_samples));
    }
}

fn frame_rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }

    let mean_square = samples
        .iter()
        .map(|sample| f64::from(*sample) * f64::from(*sample))
        .sum::<f64>()
        / samples.len() as f64;
    mean_square.sqrt() as f32
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

    /// Transcribe a bounded stream of mono 16 kHz audio.
    pub fn transcribe_stream(
        &mut self,
        source: &mut dyn AudioSource,
    ) -> Result<Vec<TranscriptSegment>> {
        let mel_filters = load_mel_filters()?;
        let mut scanner = SilenceScanner::new();
        let mut skip_stats = SilenceSkipStats::default();
        let mut last_region_end = 0usize;
        let mut segments = Vec::new();

        while let Some(chunk) = source.next_chunk()? {
            for region in scanner.push(&chunk) {
                self.transcribe_region(
                    region,
                    &mel_filters,
                    &mut last_region_end,
                    &mut skip_stats,
                    &mut segments,
                )?;
            }
        }

        for region in scanner.finish() {
            self.transcribe_region(
                region,
                &mel_filters,
                &mut last_region_end,
                &mut skip_stats,
                &mut segments,
            )?;
        }

        let total_samples = scanner.processed_samples;
        if total_samples > last_region_end {
            skip_stats.record(last_region_end, total_samples);
        }

        let mut segments = deduplicate_segments(segments);
        segments.sort_by(|a, b| {
            a.start
                .total_cmp(&b.start)
                .then_with(|| a.end.total_cmp(&b.end))
        });

        let samples_per_second = f64::from(MODEL_SAMPLE_RATE);
        info!(
            skipped_silence_sections = skip_stats.section_count,
            skipped_silence_duration_seconds =
                skip_stats.skipped_samples as f64 / samples_per_second,
            recording_duration_seconds = total_samples as f64 / samples_per_second,
            activity_frame_ms = ACTIVITY_FRAME_SAMPLES as f64 * 1000.0 / samples_per_second,
            activity_rms_threshold = ACTIVITY_RMS_THRESHOLD,
            boundary_padding_ms = SPEECH_PADDING_SAMPLES as f64 * 1000.0 / samples_per_second,
            silence_hangover_ms = SILENCE_HANGOVER_SAMPLES as f64 * 1000.0 / samples_per_second,
            max_region_seconds = MAX_REGION_SAMPLES as f64 / samples_per_second,
            region_advance_seconds = REGION_ADVANCE_SAMPLES as f64 / samples_per_second,
            "Transcription silence skipping summary"
        );

        Ok(segments)
    }

    fn transcribe_region(
        &mut self,
        region: SpeechRegion,
        mel_filters: &[f32],
        last_region_end: &mut usize,
        skip_stats: &mut SilenceSkipStats,
        output: &mut Vec<TranscriptSegment>,
    ) -> Result<()> {
        if region.start_sample > *last_region_end {
            skip_stats.record(*last_region_end, region.start_sample);
        }

        let mut local_start = 0usize;
        while local_start < region.samples.len() {
            let sample_count = (region.samples.len() - local_start).min(MAX_REGION_SAMPLES);
            let absolute_start = region.start_sample + local_start;
            output.extend(self.decode_window(
                &region.samples[local_start..local_start + sample_count],
                absolute_start,
                mel_filters,
            )?);

            if local_start + sample_count >= region.samples.len() {
                break;
            }
            local_start += REGION_ADVANCE_SAMPLES;
        }

        *last_region_end = (*last_region_end).max(region.start_sample + region.samples.len());
        Ok(())
    }

    fn decode_window(
        &mut self,
        samples: &[f32],
        window_start_samples: usize,
        mel_filters: &[f32],
    ) -> Result<Vec<TranscriptSegment>> {
        let mel = audio::pcm_to_mel(self.model.config(), samples, mel_filters);
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

        let (_, _, mel_frames) = mel.dims3()?;
        // Candle's pcm_to_mel pads its output by at least 15 seconds. Longer
        // inputs can therefore produce more frames than Whisper's positional
        // embedding supports (1500 encoder steps / 3000 mel frames).
        let content_frames = (samples.len() / m::HOP_LENGTH).min(m::N_FRAMES);
        let mel = mel.narrow(2, 0, content_frames.min(mel_frames))?;
        let mel_segment = self.pad_mel_segment(&mel, content_frames)?;
        let decoded = self.decode_segment(&mel_segment)?;
        if decoded.no_speech_prob > m::NO_SPEECH_THRESHOLD
            && decoded.avg_logprob < m::LOGPROB_THRESHOLD
        {
            return Ok(Vec::new());
        }

        let sample_rate = f64::from(u32::try_from(m::SAMPLE_RATE)?);
        let hop_length = f64::from(u32::try_from(m::HOP_LENGTH)?);
        let segment_start =
            f64::from(u32::try_from(window_start_samples)?) / f64::from(MODEL_SAMPLE_RATE);
        let segment_duration = f64::from(u32::try_from(content_frames)?) * hop_length / sample_rate;
        let timestamp_segments =
            self.extract_timestamp_segments(&decoded.tokens, segment_start, segment_duration)?;

        let mut segments = if timestamp_segments.is_empty() {
            let text = decoded.text.trim();
            if text.is_empty() {
                Vec::new()
            } else {
                vec![TranscriptSegment {
                    start: segment_start,
                    end: segment_start + segment_duration,
                    text: text.to_owned(),
                }]
            }
        } else {
            timestamp_segments
        };

        segments.retain_mut(|segment| match filter_text_artifacts(&segment.text) {
            TextArtifactFilter::Keep {
                text,
                removed_characters,
            } => {
                if removed_characters > 0 {
                    info!(
                        start = segment.start,
                        end = segment.end,
                        removed_characters,
                        "Removed formatting characters from transcript segment"
                    );
                }
                segment.text = text;
                true
            }
            TextArtifactFilter::Drop { reason } => {
                warn!(
                    start = segment.start,
                    end = segment.end,
                    reason,
                    "Removed transcript segment as a text artifact"
                );
                false
            }
        });

        Ok(segments)
    }

    pub fn is_timestamp_token(&self, token: u32) -> bool {
        token > self.no_timestamps_token
    }

    fn pad_mel_segment(&self, mel: &Tensor, segment_size: usize) -> Result<Tensor> {
        let mel_segment = mel.clone();
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

impl Transcriber for WhisperTranscriber {
    fn transcribe_stream(&mut self, audio: &mut dyn AudioSource) -> Result<Vec<TranscriptSegment>> {
        Self::transcribe_stream(self, audio)
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

fn normalized_tokens(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|word| {
            word.chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
                .to_lowercase()
        })
        .filter(|word| !word.is_empty())
        .collect()
}

fn raw_words(text: &str) -> Vec<&str> {
    text.split_whitespace().collect()
}

enum TextArtifactFilter {
    Keep {
        text: String,
        removed_characters: usize,
    },
    Drop {
        reason: &'static str,
    },
}

/// Remove obvious decoder formatting noise while preserving ordinary speech punctuation.
fn filter_text_artifacts(text: &str) -> TextArtifactFilter {
    const MAX_CONSECUTIVE_REPEATED_WORDS: usize = 16;
    const MIN_SYMBOL_HEAVY_COUNT: usize = 16;
    const MAX_SYMBOL_COUNT: usize = 40;
    const SYMBOL_HEAVY_RATIO: f64 = 0.35;

    let non_whitespace_count = text
        .chars()
        .filter(|character| !character.is_whitespace())
        .count();
    let symbol_count = text
        .chars()
        .filter(|character| !character.is_whitespace() && !character.is_alphanumeric())
        .count();
    if symbol_count >= MAX_SYMBOL_COUNT
        || (symbol_count >= MIN_SYMBOL_HEAVY_COUNT
            && non_whitespace_count > 0
            && symbol_count as f64 / non_whitespace_count as f64 >= SYMBOL_HEAVY_RATIO)
    {
        return TextArtifactFilter::Drop {
            reason: "symbol-heavy text",
        };
    }

    let mut previous_word = None;
    let mut repeated_words = 0usize;
    for word in normalized_tokens(text) {
        if previous_word.as_deref() == Some(word.as_str()) {
            repeated_words += 1;
        } else {
            previous_word = Some(word);
            repeated_words = 1;
        }
        if repeated_words >= MAX_CONSECUTIVE_REPEATED_WORDS {
            return TextArtifactFilter::Drop {
                reason: "extreme consecutive word repetition",
            };
        }
    }

    let mut output = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut removed_characters = 0usize;

    while let Some(character) = chars.next() {
        if character == '*' {
            removed_characters += 1;
            continue;
        }

        if character == ':' && chars.peek() == Some(&':') {
            removed_characters += 1;
            while chars.peek() == Some(&':') {
                chars.next();
                removed_characters += 1;
            }
            continue;
        }

        output.push(character);
    }

    let output = output.trim().to_owned();
    if !output.chars().any(char::is_alphanumeric) {
        return TextArtifactFilter::Drop {
            reason: "punctuation-only text",
        };
    }

    TextArtifactFilter::Keep {
        text: output,
        removed_characters,
    }
}

fn has_temporal_overlap(a: &TranscriptSegment, b: &TranscriptSegment) -> bool {
    a.start < b.end && b.start < a.end
}

fn contains_tokens(container: &[String], needle: &[String]) -> bool {
    needle.len() >= 3
        && container
            .windows(needle.len())
            .any(|window| window == needle)
}

fn suffix_prefix_overlap(left: &[String], right: &[String]) -> usize {
    (1..=left.len().min(right.len()))
        .rev()
        .find(|&length| left[left.len() - length..] == right[..length])
        .unwrap_or(0)
}

fn merge_overlapping_text(left: &str, right: &str) -> Option<String> {
    let left_tokens = normalized_tokens(left);
    let right_tokens = normalized_tokens(right);
    let overlap = suffix_prefix_overlap(&left_tokens, &right_tokens);

    if overlap < 3 {
        return None;
    }

    let right_words = raw_words(right);
    let suffix = right_words.get(overlap..)?.join(" ");

    Some(if suffix.is_empty() {
        left.trim().to_owned()
    } else {
        format!("{} {}", left.trim(), suffix)
    })
}

pub fn deduplicate_segments(mut segments: Vec<TranscriptSegment>) -> Vec<TranscriptSegment> {
    segments.sort_by(|a, b| {
        a.start
            .total_cmp(&b.start)
            .then_with(|| a.end.total_cmp(&b.end))
    });

    let mut output: Vec<TranscriptSegment> = Vec::new();

    for segment in segments {
        let segment_tokens = normalized_tokens(&segment.text);
        let mut duplicate_index = None;

        // A duplicate can be hidden behind another segment, so inspect every
        // retained segment whose time range overlaps this candidate.
        for index in (0..output.len()).rev() {
            let existing = &output[index];

            if !has_temporal_overlap(existing, &segment) {
                continue;
            }

            let existing_tokens = normalized_tokens(&existing.text);
            let same_text = existing_tokens == segment_tokens;
            let contained = contains_tokens(&existing_tokens, &segment_tokens)
                || contains_tokens(&segment_tokens, &existing_tokens);

            if same_text || contained {
                duplicate_index = Some(index);
                break;
            }

            if merge_overlapping_text(&existing.text, &segment.text).is_some() {
                duplicate_index = Some(index);
                break;
            }
        }

        let Some(index) = duplicate_index else {
            output.push(segment);
            continue;
        };

        let existing = &mut output[index];
        let existing_tokens = normalized_tokens(&existing.text);

        if existing_tokens == segment_tokens {
            // Identical text is usually emitted with padded timestamps in one
            // window, so retain the tighter interval.
            if (segment.end - segment.start) < (existing.end - existing.start) {
                *existing = segment;
            }
        } else if contains_tokens(&segment_tokens, &existing_tokens) {
            // A later overlapping window often completes a truncated phrase.
            // Keep the richer candidate, as in "you can, um" -> "you can
            // swap your helmet in this menu".
            *existing = segment;
        } else if let Some(text) = merge_overlapping_text(&existing.text, &segment.text) {
            existing.end = existing.end.max(segment.end);
            existing.text = text;
        }
    }

    output
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::{
        TranscriptSegment, contains_tokens, deduplicate_segments, has_temporal_overlap,
        merge_overlapping_text, normalized_tokens, raw_words, suffix_prefix_overlap,
    };

    fn segment(start: f64, end: f64, text: &str) -> TranscriptSegment {
        TranscriptSegment {
            start,
            end,
            text: text.into(),
        }
    }

    #[test]
    fn normalisation_removes_punctuation_case_and_empty_tokens() {
        assert_eq!(
            normalized_tokens(" Hello, WORLD! -- 42 "),
            vec!["hello", "world", "42"]
        );
        assert!(normalized_tokens("... --").is_empty());
        assert_eq!(raw_words(" a  b\n c "), vec!["a", "b", "c"]);
    }

    #[test]
    fn temporal_overlap_excludes_touching_boundaries() {
        assert!(has_temporal_overlap(
            &segment(0.0, 2.0, "a"),
            &segment(1.0, 3.0, "b")
        ));
        assert!(!has_temporal_overlap(
            &segment(0.0, 1.0, "a"),
            &segment(1.0, 2.0, "b")
        ));
    }

    #[test]
    fn token_containment_requires_at_least_three_tokens() {
        let container = normalized_tokens("one two three four");
        assert!(contains_tokens(
            &container,
            &normalized_tokens("two three four")
        ));
        assert!(!contains_tokens(
            &container,
            &normalized_tokens("two three")
        ));
        assert!(!contains_tokens(
            &container,
            &normalized_tokens("one three four")
        ));
    }

    #[test]
    fn finds_longest_suffix_prefix_overlap() {
        let left = normalized_tokens("one two three four");
        let right = normalized_tokens("two three four five");
        assert_eq!(suffix_prefix_overlap(&left, &right), 3);
        assert_eq!(suffix_prefix_overlap(&left, &normalized_tokens("none")), 0);
    }

    #[test]
    fn merges_text_only_for_three_or_more_overlapping_tokens() {
        assert_eq!(
            merge_overlapping_text("one two three four", "two three four five").as_deref(),
            Some("one two three four five")
        );
        assert_eq!(merge_overlapping_text("one two", "two three"), None);
        assert_eq!(
            merge_overlapping_text("One, TWO three", "one two THREE").as_deref(),
            Some("One, TWO three")
        );
    }

    #[test]
    fn deduplication_sorts_segments_by_start_then_end() {
        let output = deduplicate_segments(vec![
            segment(2.0, 3.0, "later"),
            segment(1.0, 3.0, "longer"),
            segment(1.0, 2.0, "earlier"),
        ]);
        assert_eq!(
            output
                .iter()
                .map(|item| item.text.as_str())
                .collect::<Vec<_>>(),
            vec!["earlier", "longer", "later"]
        );
    }

    #[test]
    fn identical_overlapping_text_keeps_tighter_interval() {
        let output = deduplicate_segments(vec![
            segment(0.0, 5.0, "Same text here"),
            segment(1.0, 3.0, "same, TEXT here!"),
        ]);
        assert_eq!(output.len(), 1);
        assert_eq!((output[0].start, output[0].end), (1.0, 3.0));
    }

    #[test]
    fn contained_overlapping_text_keeps_richer_candidate() {
        let output = deduplicate_segments(vec![
            segment(0.0, 3.0, "you can swap"),
            segment(1.0, 4.0, "you can swap your helmet"),
        ]);
        assert_eq!(output.len(), 1);
        assert_eq!(output[0].text, "you can swap your helmet");
    }

    #[test]
    fn partial_overlaps_are_merged_and_extend_end_time() {
        let output = deduplicate_segments(vec![
            segment(0.0, 3.0, "one two three four"),
            segment(2.0, 5.0, "two three four five"),
        ]);
        assert_eq!(output.len(), 1);
        assert_eq!(output[0].text, "one two three four five");
        assert_eq!(output[0].end, 5.0);
    }

    #[test]
    fn non_overlapping_duplicates_are_retained() {
        let output = deduplicate_segments(vec![
            segment(0.0, 1.0, "same text here"),
            segment(1.0, 2.0, "same text here"),
        ]);
        assert_eq!(output.len(), 2);
    }

    #[test]
    fn duplicate_can_be_found_behind_an_unrelated_segment() {
        let output = deduplicate_segments(vec![
            segment(0.0, 5.0, "duplicate phrase here"),
            segment(1.0, 4.0, "unrelated words remain"),
            segment(2.0, 3.0, "duplicate phrase here"),
        ]);
        assert_eq!(output.len(), 2);
        assert_eq!(output[0].start, 2.0);
    }
}
