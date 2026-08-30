use anyhow::{Result, anyhow};
use tokenizers::Tokenizer;

pub(super) fn content_token_len(tokenizer: &Tokenizer, text: &str) -> Result<usize> {
    tokenizer
        .encode(text, true)
        .map(|encoding| {
            encoding
                .get_offsets()
                .iter()
                .filter(|(start, end)| end > start)
                .count()
        })
        .map_err(|error| anyhow!("Failed to count content tokens: {error}"))
}
pub(super) fn encoded_len(tokenizer: &Tokenizer, text: &str) -> Result<usize> {
    tokenizer
        .encode(text, true)
        .map(|encoding| encoding.len())
        .map_err(|error| anyhow!("Failed to tokenize chunk: {error}"))
}
