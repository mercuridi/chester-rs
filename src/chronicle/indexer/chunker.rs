use anyhow::{Result, anyhow};
use tokenizers::Tokenizer;

use super::document::{Chunk, Document};

/// Split a document into chunks whose encoded representation fits the embedding model.
///
/// Source ranges are used when a long paragraph must be split, so chunk text is never
/// reconstructed from decoded tokens (which can change whitespace or Unicode text).
pub fn chunk(document: &Document, tokenizer: &Tokenizer, max_tokens: usize) -> Result<Vec<Chunk>> {
    if max_tokens < 3 {
        return Err(anyhow!("Chunk token budget must be at least 3"));
    }

    if document.content.trim().is_empty() {
        return Ok(Vec::new());
    }

    let paragraphs = document
        .content
        .split_inclusive("\n\n")
        .scan(0, |start, paragraph| {
            let range = (*start, *start + paragraph.len());
            *start = range.1;
            Some(range)
        })
        .filter(|(start, end)| !document.content[*start..*end].trim().is_empty())
        .collect::<Vec<_>>();

    let mut ranges = Vec::new();
    let mut current = None::<(usize, usize)>;

    for (start, end) in paragraphs {
        let candidate = current.map_or_else(
            || document.content[start..end].to_owned(),
            |(current_start, _)| document.content[current_start..end].to_owned(),
        );

        if encoded_len(tokenizer, &candidate)? <= max_tokens {
            current = Some((
                current.map_or(start, |(current_start, _)| current_start),
                end,
            ));
            continue;
        }

        if let Some((current_start, current_end)) = current.take() {
            ranges.push((current_start, current_end));
        }

        let paragraph = &document.content[start..end];
        if encoded_len(tokenizer, paragraph)? <= max_tokens {
            current = Some((start, end));
        } else {
            ranges.extend(
                split_long_text(paragraph, tokenizer, max_tokens)?
                    .into_iter()
                    .map(|(local_start, local_end)| (start + local_start, start + local_end)),
            );
        }
    }

    if let Some(range) = current {
        ranges.push(range);
    }

    Ok(ranges
        .into_iter()
        .enumerate()
        .map(|(index, (start, end))| Chunk {
            document_path: document.path.clone(),
            index,
            content: document.content[start..end].to_owned(),
            heading: None,
        })
        .collect())
}

fn encoded_len(tokenizer: &Tokenizer, text: &str) -> Result<usize> {
    tokenizer
        .encode(text, true)
        .map(|encoding| encoding.len())
        .map_err(|error| anyhow!("Failed to tokenize chunk: {error}"))
}

fn split_long_text(
    text: &str,
    tokenizer: &Tokenizer,
    max_tokens: usize,
) -> Result<Vec<(usize, usize)>> {
    let mut ranges = Vec::new();
    let mut start = 0;

    while start < text.len() {
        let remaining = &text[start..];
        if encoded_len(tokenizer, remaining)? <= max_tokens {
            ranges.push((start, text.len()));
            break;
        }

        let encoding = tokenizer
            .encode(remaining, true)
            .map_err(|error| anyhow!("Failed to tokenize long chunk: {error}"))?;
        let offsets = encoding
            .get_offsets()
            .iter()
            .copied()
            .filter(|(offset_start, offset_end)| offset_end > offset_start)
            .collect::<Vec<_>>();

        let content_budget = max_tokens.saturating_sub(encoding.len() - offsets.len());
        let mut end = offsets
            .iter()
            .take(content_budget.max(1))
            .next_back()
            .map_or(remaining.len(), |(_, offset_end)| *offset_end);

        while end > 0 && encoded_len(tokenizer, &remaining[..end])? > max_tokens {
            end = offsets
                .iter()
                .take_while(|(_, offset_end)| *offset_end < end)
                .last()
                .map_or(0, |(_, offset_end)| *offset_end);
        }

        if end == 0 {
            return Err(anyhow!(
                "Unable to split text into a chunk of {max_tokens} tokens"
            ));
        }

        ranges.push((start, start + end));
        start += end;
    }

    Ok(ranges)
}
