use anyhow::{Result, anyhow};
use tokenizers::Tokenizer;

use super::text::{sentence_ranges, word_ranges};
use super::tokenizer::{content_token_len, encoded_len};
use super::types::{BlockKind, OverlapInfo, PlannedChunk};

pub(super) fn apply_overlap(
    chunks: &mut [PlannedChunk],
    source: &str,
    tokenizer: &Tokenizer,
    max_tokens: usize,
    overlap_tokens: usize,
) -> Result<Vec<OverlapInfo>> {
    let mut overlap = vec![OverlapInfo::default(); chunks.len()];
    if overlap_tokens == 0 {
        return Ok(overlap);
    }

    for index in 1..chunks.len() {
        let (previous, current) = chunks.split_at_mut(index);
        let previous = &previous[index - 1];
        let current = &mut current[0];

        if previous.section != current.section || matches!(previous.kind, BlockKind::Heading(_)) {
            continue;
        }
        overlap[index].eligible = true;

        let previous_text = &source[previous.range.clone()];
        let mut candidates =
            overlap_candidates(previous_text, previous.kind, tokenizer, overlap_tokens)?;
        candidates.sort_by_key(|(_, token_count)| std::cmp::Reverse(*token_count));

        for (local_start, token_count) in candidates {
            let overlap_start = previous.range.start + local_start;
            if overlap_start >= current.range.start
                || encoded_len(tokenizer, &source[overlap_start..current.range.end])? > max_tokens
            {
                continue;
            }

            current.range.start = overlap_start;
            overlap[index].tokens = token_count;
            break;
        }
    }

    Ok(overlap)
}

fn overlap_candidates(
    text: &str,
    kind: BlockKind,
    tokenizer: &Tokenizer,
    overlap_tokens: usize,
) -> Result<Vec<(usize, usize)>> {
    let mut candidates = Vec::new();
    let mut boundaries = Vec::new();

    if matches!(
        kind,
        BlockKind::Paragraph | BlockKind::BlockQuote | BlockKind::ListItem
    ) {
        boundaries.extend(sentence_ranges(text).into_iter().map(|range| range.start));
        boundaries.extend(word_ranges(text).into_iter().map(|range| range.start));
    }

    let encoding = tokenizer
        .encode(text, true)
        .map_err(|error| anyhow!("Failed to tokenize overlap: {error}"))?;
    boundaries.extend(
        encoding
            .get_offsets()
            .iter()
            .filter_map(|(start, end)| (end > start).then_some(*start)),
    );
    boundaries.sort_unstable();
    boundaries.dedup();

    for start in boundaries {
        let token_count = content_token_len(tokenizer, &text[start..])?;
        if token_count <= overlap_tokens {
            candidates.push((start, token_count));
        }
    }

    Ok(candidates)
}
