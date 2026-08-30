use crate::chronicle::transcription::whisper::transcriber::{
    TranscriptSegment, WhisperTranscriber,
};

use anyhow::{Result, anyhow};
use tokenizers::Tokenizer;

pub fn token_id(tokenizer: &Tokenizer, token: &str) -> Result<u32> {
    tokenizer
        .token_to_id(token)
        .ok_or_else(|| anyhow!("No tokenizer ID for {token}"))
}

impl WhisperTranscriber {
    pub fn extract_timestamp_segments(
        &self,
        tokens: &[u32],
        segment_start: f64,
        segment_duration: f64,
    ) -> Result<Vec<TranscriptSegment>> {
        let mut output = Vec::new();
        let mut text_tokens = Vec::new();
        let mut start_timestamp = None;

        for &token in tokens {
            if token == self.sot_token
                || token == self.eot_token
                || token == self.language_token
                || token == self.transcribe_token
            {
                continue;
            }

            if self.is_timestamp_token(token) {
                let timestamp = f64::from(token - self.no_timestamps_token + 1) * 0.02;

                if let Some(start) = start_timestamp {
                    if !text_tokens.is_empty() {
                        let text = self
                            .tokenizer
                            .decode(&text_tokens, true)
                            .map_err(|e| anyhow!("Tokenizer decode failed: {e}"))?
                            .trim()
                            .to_owned();

                        if !text.is_empty() && timestamp >= start {
                            output.push(TranscriptSegment {
                                start: segment_start + start,
                                end: segment_start + timestamp,
                                text,
                            });
                        }
                    }

                    text_tokens.clear();
                }

                start_timestamp = Some(timestamp);
            } else {
                text_tokens.push(token);
            }
        }

        if !text_tokens.is_empty() {
            let text = self
                .tokenizer
                .decode(&text_tokens, true)
                .map_err(|e| anyhow!("Tokenizer decode failed: {e}"))?
                .trim()
                .to_owned();

            if !text.is_empty() {
                output.push(TranscriptSegment {
                    start: segment_start + start_timestamp.unwrap_or(0.0),
                    end: segment_start + segment_duration,
                    text,
                });
            }
        }

        Ok(output)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::token_id;
    use tokenizers::{Tokenizer, models::wordlevel::WordLevel};

    fn tokenizer() -> anyhow::Result<Tokenizer> {
        let model = WordLevel::builder()
            .vocab([("known".to_owned(), 7)].into_iter().collect())
            .unk_token("[UNK]".to_owned())
            .build()
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        Ok(Tokenizer::new(model))
    }

    #[test]
    fn returns_known_token_id() -> anyhow::Result<()> {
        assert_eq!(token_id(&tokenizer()?, "known")?, 7);
        Ok(())
    }

    #[test]
    fn reports_unknown_token() -> anyhow::Result<()> {
        let error = token_id(&tokenizer()?, "missing").unwrap_err();
        assert!(error.to_string().contains("No tokenizer ID for missing"));
        Ok(())
    }
}
