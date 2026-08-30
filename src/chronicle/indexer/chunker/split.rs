use std::ops::Range;

use super::text::{sentence_ranges, word_ranges};
use super::tokenizer::encoded_len;
use super::types::BlockKind;
use anyhow::{Result, anyhow};
use tokenizers::Tokenizer;

pub(super) fn split_block(
    text: &str,
    kind: BlockKind,
    tokenizer: &Tokenizer,
    first_budget: usize,
    continuation_budget: usize,
) -> Result<Vec<Range<usize>>> {
    let first_pass = split_block_with_budget(text, kind, tokenizer, first_budget)?;
    if first_pass.len() <= 1 || first_budget == continuation_budget {
        return Ok(first_pass);
    }

    let first = first_pass[0].clone();
    let remainder_start = first.end;
    let mut ranges = vec![first];
    ranges.extend(
        split_block_with_budget(
            &text[remainder_start..],
            kind,
            tokenizer,
            continuation_budget,
        )?
        .into_iter()
        .map(|range| remainder_start + range.start..remainder_start + range.end),
    );
    Ok(ranges)
}

fn split_block_with_budget(
    text: &str,
    kind: BlockKind,
    tokenizer: &Tokenizer,
    max_tokens: usize,
) -> Result<Vec<Range<usize>>> {
    match kind {
        BlockKind::Paragraph | BlockKind::BlockQuote | BlockKind::ListItem => {
            split_semantic_text(text, tokenizer, max_tokens)
        }
        BlockKind::Heading(_) | BlockKind::CodeBlock | BlockKind::Table => {
            split_long_text(text, tokenizer, max_tokens)
        }
    }
}

fn split_semantic_text(
    text: &str,
    tokenizer: &Tokenizer,
    max_tokens: usize,
) -> Result<Vec<Range<usize>>> {
    let mut ranges = Vec::new();
    let mut sentence_units = Vec::new();

    for sentence in sentence_ranges(text) {
        if encoded_len(tokenizer, &text[sentence.clone()])? <= max_tokens {
            sentence_units.push(sentence);
        } else {
            ranges.extend(pack_units(
                text,
                std::mem::take(&mut sentence_units),
                tokenizer,
                max_tokens,
            )?);
            ranges.extend(pack_units(
                text,
                word_ranges(&text[sentence.clone()]),
                tokenizer,
                max_tokens,
            )?);
        }
    }

    ranges.extend(pack_units(text, sentence_units, tokenizer, max_tokens)?);
    Ok(ranges)
}

fn pack_units(
    text: &str,
    units: Vec<Range<usize>>,
    tokenizer: &Tokenizer,
    max_tokens: usize,
) -> Result<Vec<Range<usize>>> {
    let mut ranges = Vec::new();
    let mut current = None::<Range<usize>>;

    for unit in units {
        if encoded_len(tokenizer, &text[unit.clone()])? > max_tokens {
            if let Some(range) = current.take() {
                ranges.push(range);
            }
            ranges.extend(
                split_long_text(&text[unit.clone()], tokenizer, max_tokens)?
                    .into_iter()
                    .map(|range| unit.start + range.start..unit.start + range.end),
            );
            continue;
        }

        let candidate = current
            .as_ref()
            .map_or_else(|| unit.clone(), |range| range.start..unit.end);
        if encoded_len(tokenizer, &text[candidate.clone()])? <= max_tokens {
            current = Some(candidate);
        } else {
            if let Some(range) = current.take() {
                ranges.push(range);
            }
            current = Some(unit);
        }
    }

    if let Some(range) = current {
        ranges.push(range);
    }
    Ok(ranges)
}

fn split_long_text(
    text: &str,
    tokenizer: &Tokenizer,
    max_tokens: usize,
) -> Result<Vec<Range<usize>>> {
    let mut ranges = Vec::new();
    let mut start = 0;

    while start < text.len() {
        let remaining = &text[start..];
        if encoded_len(tokenizer, remaining)? <= max_tokens {
            ranges.push(start..text.len());
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

        ranges.push(start..start + end);
        start += end;
    }

    Ok(ranges)
}
