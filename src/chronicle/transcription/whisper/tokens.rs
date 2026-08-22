use crate::chronicle::transcription::whisper::transcriber::{TranscriptSegment, WhisperTranscriber};

use anyhow::{anyhow, Result};
use candle_transformers::models::whisper::{self as m};
use tokenizers::Tokenizer;

pub fn token_id(
    tokenizer: &Tokenizer,
    token: &str,
) -> Result<u32> {
    tokenizer
        .token_to_id(token)
        .ok_or_else(|| anyhow!("No tokenizer ID for {token}"))
}

impl WhisperTranscriber {
    pub fn extract_timestamp_segments(
        &self,
        tokens: &[u32],
        segment_start: f64,
    ) -> Result<Vec<TranscriptSegment>> {
        let mut output = Vec::new();
        let mut text_tokens = Vec::new();
        let mut previous_timestamp = 0.0f64;

        for &token in tokens {
            if token == self.sot_token
                || token == self.eot_token
            {
                continue;
            }

            if token > self.no_timestamps_token {
                let timestamp =
                    (token - self.no_timestamps_token + 1)
                        as f64
                        / 50.0;

                if !text_tokens.is_empty() {
                    let text = self
                        .tokenizer
                        .decode(&text_tokens, true)
                        .map_err(|error| {
                            anyhow!(
                                "Tokenizer decode failed: {error}"
                            )
                        })?
                        .trim()
                        .to_owned();

                    if !text.is_empty() {
                        output.push(TranscriptSegment {
                            start: segment_start
                                + previous_timestamp,
                            end: segment_start + timestamp,
                            text,
                        });
                    }

                    text_tokens.clear();
                }

                previous_timestamp = timestamp;
            } else {
                text_tokens.push(token);
            }
        }

        if !text_tokens.is_empty() {
            let text = self
                .tokenizer
                .decode(&text_tokens, true)
                .map_err(|error| {
                    anyhow!("Tokenizer decode failed: {error}")
                })?
                .trim()
                .to_owned();

            if !text.is_empty() {
                output.push(TranscriptSegment {
                    start: segment_start + previous_timestamp,
                    end: segment_start + m::CHUNK_LENGTH as f64,
                    text,
                });
            }
        }

        Ok(output)
    }
}