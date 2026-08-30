use anyhow::{Context, Result, anyhow};
use candle_core::{Device, Tensor};
use candle_transformers::models::whisper::{self as m, audio};
use tokenizers::Tokenizer;

use crate::chronicle::transcription::audio::Audio;
use crate::chronicle::transcription::constants::MODEL_SAMPLE_RATE;
use crate::chronicle::transcription::whisper::model::Model;

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
        let stride_frames = 25 * m::SAMPLE_RATE / m::HOP_LENGTH;
        let sample_rate = f64::from(u32::try_from(m::SAMPLE_RATE)?);
        let hop_length = f64::from(u32::try_from(m::HOP_LENGTH)?);

        let (_, _, content_frames) = mel.dims3()?;

        let mut seek = 0;
        let mut segments = Vec::new();

        while seek < content_frames {
            let segment_size = usize::min(content_frames - seek, m::N_FRAMES);

            let mel_segment = self.pad_mel_segment(mel, segment_size, seek)?;

            let segment_start = f64::from(u32::try_from(seek)?) * hop_length / sample_rate;

            let segment_duration =
                f64::from(u32::try_from(segment_size)?) * hop_length / sample_rate;

            let decoded = self.decode_segment(&mel_segment)?;

            if decoded.no_speech_prob > m::NO_SPEECH_THRESHOLD
                && decoded.avg_logprob < m::LOGPROB_THRESHOLD
            {
                // Advance past rejected windows as well. Otherwise a silent
                // window causes the decoder to process the same window forever.
                if segment_size == content_frames - seek {
                    break;
                }

                seek += stride_frames;
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

impl crate::chronicle::transcription::service::Transcriber for WhisperTranscriber {
    fn transcribe(&mut self, audio: &Audio) -> Result<Vec<TranscriptSegment>> {
        Self::transcribe(self, audio)
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
