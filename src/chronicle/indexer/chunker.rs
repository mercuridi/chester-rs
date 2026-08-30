use std::ops::Range;

use anyhow::{Result, anyhow};
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag};
use tokenizers::Tokenizer;
use unicode_segmentation::UnicodeSegmentation;

use super::document::{Chunk, Document};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Paragraph,
    Heading(HeadingLevel),
    BlockQuote,
    CodeBlock,
    ListItem,
    Table,
}

#[derive(Debug)]
struct ParsedBlock {
    range: Range<usize>,
    kind: BlockKind,
    section: usize,
    heading: Option<String>,
}

#[derive(Debug)]
struct PlannedChunk {
    range: Range<usize>,
    kind: BlockKind,
    section: usize,
    heading: Option<String>,
    token_budget: usize,
}

#[derive(Debug, Default, Clone, Copy)]
struct OverlapInfo {
    eligible: bool,
    tokens: usize,
}

/// Split Markdown into token-bounded chunks while retaining the original source text.
pub fn chunk(
    document: &Document,
    tokenizer: &Tokenizer,
    max_tokens: usize,
    overlap_tokens: usize,
) -> Result<Vec<Chunk>> {
    validate_budgets(max_tokens, overlap_tokens)?;

    if document.content.trim().is_empty() {
        return Ok(Vec::new());
    }

    let blocks = parse_blocks(&document.content);
    let mut chunks = Vec::new();
    let mut current = None::<PlannedChunk>;

    for block in blocks {
        if matches!(block.kind, BlockKind::Heading(_))
            && let Some(chunk) = current.take()
        {
            chunks.push(chunk);
        }

        let candidate_start = current
            .as_ref()
            .map_or(block.range.start, |chunk| chunk.range.start);
        let candidate = &document.content[candidate_start..block.range.end];
        let candidate_budget = current.as_ref().map_or_else(
            || next_chunk_budget(&chunks, block.section, max_tokens, overlap_tokens),
            |chunk| chunk.token_budget,
        );

        if encoded_len(tokenizer, candidate)? <= candidate_budget {
            current = Some(PlannedChunk {
                range: candidate_start..block.range.end,
                section: block.section,
                kind: block.kind,
                heading: block.heading,
                token_budget: candidate_budget,
            });
            continue;
        }

        if let Some(chunk) = current.take() {
            chunks.push(chunk);
        }

        let token_budget = next_chunk_budget(&chunks, block.section, max_tokens, overlap_tokens);
        let text = &document.content[block.range.clone()];
        if encoded_len(tokenizer, text)? <= token_budget {
            current = Some(PlannedChunk {
                range: block.range,
                section: block.section,
                kind: block.kind,
                heading: block.heading,
                token_budget,
            });
        } else {
            let continuation_budget = continuation_budget(block.kind, max_tokens, overlap_tokens);
            let split = split_block(
                text,
                block.kind,
                tokenizer,
                token_budget,
                continuation_budget,
            )?;
            chunks.extend(
                split
                    .into_iter()
                    .enumerate()
                    .map(|(index, range)| PlannedChunk {
                        range: block.range.start + range.start..block.range.start + range.end,
                        section: block.section,
                        kind: block.kind,
                        heading: block.heading.clone(),
                        token_budget: if index == 0 {
                            token_budget
                        } else {
                            continuation_budget
                        },
                    }),
            );
        }
    }

    if let Some(chunk) = current {
        chunks.push(chunk);
    }

    let overlap = apply_overlap(
        &mut chunks,
        &document.content,
        tokenizer,
        max_tokens,
        overlap_tokens,
    )?;

    Ok(chunks
        .into_iter()
        .zip(overlap)
        .enumerate()
        .map(|(index, (chunk, overlap))| Chunk {
            document_path: document.path.clone(),
            index,
            content: document.content[chunk.range].to_owned(),
            heading: chunk.heading,
            overlap_eligible: overlap.eligible,
            overlap_tokens: overlap.tokens,
        })
        .collect())
}

fn validate_budgets(max_tokens: usize, overlap_tokens: usize) -> Result<()> {
    if max_tokens < 3 {
        return Err(anyhow!("Chunk token budget must be at least 3"));
    }
    if overlap_tokens > max_tokens.saturating_sub(3) {
        return Err(anyhow!(
            "Chunk overlap must be no greater than the chunk budget minus 3"
        ));
    }

    Ok(())
}

fn next_chunk_budget(
    chunks: &[PlannedChunk],
    section: usize,
    max_tokens: usize,
    overlap_tokens: usize,
) -> usize {
    if chunks.last().is_some_and(|chunk| {
        chunk.section == section && !matches!(chunk.kind, BlockKind::Heading(_))
    }) {
        max_tokens - overlap_tokens
    } else {
        max_tokens
    }
}

fn continuation_budget(kind: BlockKind, max_tokens: usize, overlap_tokens: usize) -> usize {
    if matches!(kind, BlockKind::Heading(_)) {
        max_tokens
    } else {
        max_tokens - overlap_tokens
    }
}

fn apply_overlap(
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

fn content_token_len(tokenizer: &Tokenizer, text: &str) -> Result<usize> {
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

fn parse_blocks(source: &str) -> Vec<ParsedBlock> {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_HEADING_ATTRIBUTES;
    let parser = Parser::new_ext(source, options).into_offset_iter();
    let mut open_blocks = Vec::<(BlockKind, usize)>::new();
    let mut blocks = Vec::new();
    let mut heading_path = Vec::<(HeadingLevel, String)>::new();
    let mut heading_capture = None::<(HeadingLevel, String)>;
    let mut section = 0;

    for (event, range) in parser {
        match event {
            Event::Start(tag) => {
                if let Tag::Heading(level, ..) = &tag {
                    heading_capture = Some((*level, String::new()));
                }

                if let Some(kind) = block_kind(&tag) {
                    open_blocks.push((kind, range.start));
                }
            }
            Event::End(tag) => {
                if let Tag::Heading(level, ..) = &tag {
                    let heading = heading_capture
                        .take()
                        .map(|(_, text)| text.trim().to_owned());
                    if let Some(heading) = heading.filter(|text| !text.is_empty()) {
                        update_heading_path(&mut heading_path, *level, heading);
                        section += 1;
                    }
                }

                if let Some(kind) = block_kind(&tag)
                    && open_blocks
                        .last()
                        .is_some_and(|(open_kind, _)| *open_kind == kind)
                {
                    let Some((_, start)) = open_blocks.pop() else {
                        continue;
                    };
                    if !open_blocks.is_empty() {
                        continue;
                    }
                    let heading = match kind {
                        BlockKind::Heading(level) => heading_path
                            .iter()
                            .find(|(path_level, _)| *path_level == level)
                            .map(|_| format_heading_path(&heading_path)),
                        _ => {
                            Some(format_heading_path(&heading_path)).filter(|path| !path.is_empty())
                        }
                    };
                    blocks.push(ParsedBlock {
                        range: start..range.end,
                        kind,
                        section,
                        heading,
                    });
                }
            }
            Event::Text(text) | Event::Code(text) | Event::Html(text) => {
                if let Some((_, heading)) = &mut heading_capture {
                    heading.push_str(&text);
                }
            }
            Event::Rule if open_blocks.is_empty() => blocks.push(ParsedBlock {
                range,
                kind: BlockKind::Paragraph,
                section,
                heading: Some(format_heading_path(&heading_path)).filter(|path| !path.is_empty()),
            }),
            _ => {}
        }
    }

    cover_source(source, blocks)
}

fn cover_source(source: &str, mut blocks: Vec<ParsedBlock>) -> Vec<ParsedBlock> {
    blocks.sort_by_key(|block| block.range.start);
    if blocks.is_empty() {
        return vec![ParsedBlock {
            range: 0..source.len(),
            kind: BlockKind::Paragraph,
            section: 0,
            heading: None,
        }];
    }

    let mut covered = Vec::<ParsedBlock>::with_capacity(blocks.len());
    let mut cursor = 0;

    for mut block in blocks {
        if block.range.end <= cursor {
            continue;
        }
        if block.range.start < cursor {
            block.range.start = cursor;
        }

        if cursor < block.range.start {
            let gap = cursor..block.range.start;
            if source[gap.clone()].trim().is_empty() {
                if let Some(previous) = covered.last_mut() {
                    previous.range.end = gap.end;
                } else {
                    block.range.start = 0;
                }
            } else {
                let (section, heading) = covered.last().map_or_else(
                    || (block.section, block.heading.clone()),
                    |previous| (previous.section, previous.heading.clone()),
                );
                covered.push(ParsedBlock {
                    range: gap,
                    kind: BlockKind::Paragraph,
                    section,
                    heading,
                });
            }
        }

        cursor = block.range.end;
        covered.push(block);
    }

    if cursor < source.len() {
        let gap = cursor..source.len();
        if source[gap.clone()].trim().is_empty() {
            if let Some(previous) = covered.last_mut() {
                previous.range.end = source.len();
            }
        } else {
            let (section, heading) = covered.last().map_or((0, None), |previous| {
                (previous.section, previous.heading.clone())
            });
            covered.push(ParsedBlock {
                range: gap,
                kind: BlockKind::Paragraph,
                section,
                heading,
            });
        }
    }

    covered
}

fn block_kind(tag: &Tag<'_>) -> Option<BlockKind> {
    match tag {
        Tag::Paragraph => Some(BlockKind::Paragraph),
        Tag::Heading(level, ..) => Some(BlockKind::Heading(*level)),
        Tag::BlockQuote => Some(BlockKind::BlockQuote),
        Tag::CodeBlock(_) => Some(BlockKind::CodeBlock),
        Tag::Item => Some(BlockKind::ListItem),
        Tag::Table(_) => Some(BlockKind::Table),
        _ => None,
    }
}

fn update_heading_path(path: &mut Vec<(HeadingLevel, String)>, level: HeadingLevel, text: String) {
    let level_number = level as u8;
    path.retain(|(existing_level, _)| (*existing_level as u8) < level_number);
    path.push((level, text));
}

fn format_heading_path(path: &[(HeadingLevel, String)]) -> String {
    path.iter()
        .map(|(_, text)| text.as_str())
        .collect::<Vec<_>>()
        .join(" > ")
}

fn encoded_len(tokenizer: &Tokenizer, text: &str) -> Result<usize> {
    tokenizer
        .encode(text, true)
        .map(|encoding| encoding.len())
        .map_err(|error| anyhow!("Failed to tokenize chunk: {error}"))
}

fn split_block(
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

fn sentence_ranges(text: &str) -> Vec<Range<usize>> {
    text.split_sentence_bound_indices()
        .map(|(start, sentence)| start..start + sentence.len())
        .collect()
}

fn word_ranges(text: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = 0;
    let mut in_word = false;

    for (index, character) in text.char_indices() {
        if !in_word && !character.is_whitespace() {
            in_word = true;
        } else if in_word && character.is_whitespace() {
            ranges.push(start..index);
            start = index;
            in_word = false;
        }
    }

    if start < text.len() {
        ranges.push(start..text.len());
    }
    ranges
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokenizers::{
        Tokenizer, models::wordlevel::WordLevel, pre_tokenizers::whitespace::Whitespace,
        processors::template::TemplateProcessing,
    };

    fn test_tokenizer() -> Result<Tokenizer> {
        let model = WordLevel::builder()
            .vocab(
                [
                    ("[UNK]".to_owned(), 0),
                    ("[CLS]".to_owned(), 1),
                    ("[SEP]".to_owned(), 2),
                    ("word".to_owned(), 3),
                ]
                .into_iter()
                .collect(),
            )
            .unk_token("[UNK]".to_owned())
            .build()
            .map_err(|error| anyhow!("Failed to build test tokenizer model: {error}"))?;
        let mut tokenizer = Tokenizer::new(model);
        tokenizer.with_pre_tokenizer(Some(Whitespace {}));
        tokenizer.with_post_processor(Some(
            TemplateProcessing::builder()
                .try_single("[CLS] $A [SEP]")
                .map_err(|error| anyhow!("Failed to build test tokenizer template: {error}"))?
                .special_tokens(vec![("[CLS]", 1), ("[SEP]", 2)])
                .build()
                .map_err(|error| anyhow!("Failed to build test tokenizer processor: {error}"))?,
        ));
        Ok(tokenizer)
    }

    fn document_with_words(word_count: usize) -> Document {
        Document {
            path: "tokens.md".into(),
            content: vec!["word"; word_count].join(" "),
            content_hash: String::new(),
        }
    }

    fn assert_full_block_coverage(source: &str, blocks: &[ParsedBlock]) {
        assert!(!blocks.is_empty());
        assert_eq!(blocks[0].range.start, 0);
        assert_eq!(
            blocks.last().map(|block| block.range.end),
            Some(source.len())
        );
        assert!(
            blocks
                .windows(2)
                .all(|pair| pair[0].range.end == pair[1].range.start)
        );
        assert_eq!(
            blocks
                .iter()
                .map(|block| &source[block.range.clone()])
                .collect::<String>(),
            source
        );
    }

    #[test]
    fn uses_unicode_sentence_boundaries_without_losing_source_text() {
        let source = "A price is 3.14. Visit https://example.com/docs. これは文です。次の文です。Unfinished prose";

        let ranges = sentence_ranges(source);

        assert!(!ranges.is_empty());
        assert_eq!(ranges[0].start, 0);
        assert_eq!(ranges.last().map(|range| range.end), Some(source.len()));
        assert!(ranges.windows(2).all(|pair| pair[0].end == pair[1].start));
        assert_eq!(
            ranges
                .iter()
                .map(|range| &source[range.clone()])
                .collect::<String>(),
            source
        );
        assert!(
            ranges
                .iter()
                .any(|range| &source[range.clone()] == "これは文です。")
        );
        assert!(
            ranges
                .iter()
                .any(|range| &source[range.clone()] == "次の文です。")
        );
    }

    #[test]
    fn parses_markdown_blocks_and_heading_hierarchy() {
        let source = "# Top\n\nIntro [link](https://example.com) ![image](image.png)\n\n## Nested\n\n- one\n- two\n\n> quoted\n\n| Name | Value |\n| --- | --- |\n| A | B |\n\n---\n\n```rust\nlet answer = 42;\n```\n";

        let blocks = parse_blocks(source);

        assert_eq!(blocks.len(), 9);
        assert_full_block_coverage(source, &blocks);
        assert_eq!(source[blocks[0].range.clone()].trim_end(), "# Top");
        assert_eq!(blocks[0].heading.as_deref(), Some("Top"));
        assert_eq!(
            source[blocks[1].range.clone()].trim_end(),
            "Intro [link](https://example.com) ![image](image.png)"
        );
        assert_eq!(blocks[1].heading.as_deref(), Some("Top"));
        assert_eq!(source[blocks[2].range.clone()].trim_end(), "## Nested");
        assert_eq!(blocks[2].heading.as_deref(), Some("Top > Nested"));
        assert!(source[blocks[3].range.clone()].starts_with("- one"));
        assert!(source[blocks[4].range.clone()].starts_with("- two"));
        assert!(source[blocks[5].range.clone()].starts_with("> quoted"));
        assert!(source[blocks[6].range.clone()].starts_with("| Name | Value |"));
        assert_eq!(source[blocks[7].range.clone()].trim_end(), "---");
        assert!(source[blocks[8].range.clone()].starts_with("```rust\n"));
        assert_eq!(blocks[8].heading.as_deref(), Some("Top > Nested"));
    }

    #[test]
    fn preserves_nested_markdown_and_unemitted_markdown_syntax() -> Result<()> {
        let source = "# Nested structures\n\n- parent\n  - nested one\n  - nested two\n\n- sibling\n\n> outer quote\n>\n> > nested quote\n>\n> outer tail\n\nA [reference][id].\n\n[id]: https://example.com\n\n[^note]: footnote text\n";
        let blocks = parse_blocks(source);

        assert_full_block_coverage(source, &blocks);
        let parent = blocks
            .iter()
            .map(|block| &source[block.range.clone()])
            .find(|text| text.contains("- parent"))
            .ok_or_else(|| anyhow!("Parent list item was not parsed"))?;
        assert!(parent.contains("nested one"));
        assert!(parent.contains("nested two"));
        let quote = blocks
            .iter()
            .map(|block| &source[block.range.clone()])
            .find(|text| text.contains("> outer quote"))
            .ok_or_else(|| anyhow!("Outer block quote was not parsed"))?;
        assert!(quote.contains("> > nested quote"));
        assert!(quote.contains("> outer tail"));

        let tokenizer = test_tokenizer()?;
        let document = Document {
            path: "nested.md".into(),
            content: source.to_owned(),
            content_hash: String::new(),
        };
        let chunks = chunk(&document, &tokenizer, 512, 0)?;
        assert_eq!(
            chunks
                .iter()
                .map(|chunk| chunk.content.as_str())
                .collect::<String>(),
            source
        );
        Ok(())
    }

    #[test]
    fn respects_the_exact_512_token_boundary() -> Result<()> {
        let tokenizer = test_tokenizer()?;
        let below_limit = document_with_words(509);
        let document = document_with_words(510);

        assert_eq!(encoded_len(&tokenizer, &below_limit.content)?, 511);
        assert_eq!(chunk(&below_limit, &tokenizer, 512, 0)?.len(), 1);
        assert_eq!(encoded_len(&tokenizer, &document.content)?, 512);
        let chunks = chunk(&document, &tokenizer, 512, 0)?;

        assert_eq!(chunks.len(), 1);
        assert_eq!(encoded_len(&tokenizer, &chunks[0].content)?, 512);
        Ok(())
    }

    #[test]
    fn reserves_and_reports_the_requested_overlap() -> Result<()> {
        let tokenizer = test_tokenizer()?;
        let document = document_with_words(40);

        let chunks = chunk(&document, &tokenizer, 12, 3)?;

        assert!(chunks.len() > 2);
        assert!(!chunks[0].overlap_eligible);
        assert_eq!(chunks[0].overlap_tokens, 0);
        assert_eq!(encoded_len(&tokenizer, &chunks[0].content)?, 12);
        for pair in chunks.windows(2) {
            let previous_words = pair[0].content.split_whitespace().collect::<Vec<_>>();
            let current_words = pair[1].content.split_whitespace().collect::<Vec<_>>();

            assert!(pair[1].overlap_eligible);
            assert_eq!(pair[1].overlap_tokens, 3);
            assert_eq!(
                previous_words[previous_words.len() - 3..],
                current_words[..3]
            );
            assert!(encoded_len(&tokenizer, &pair[1].content)? <= 12);
        }
        Ok(())
    }

    #[test]
    fn does_not_overlap_across_markdown_sections() -> Result<()> {
        let tokenizer = test_tokenizer()?;
        let words = vec!["word"; 20].join(" ");
        let document = Document {
            path: "sections.md".into(),
            content: format!("# One\n\n{words}\n\n# Two\n\n{words}"),
            content_hash: String::new(),
        };

        let chunks = chunk(&document, &tokenizer, 12, 3)?;
        let second_section_start = chunks
            .iter()
            .position(|chunk| chunk.heading.as_deref() == Some("Two"))
            .ok_or_else(|| anyhow!("Second Markdown section was not chunked"))?;

        assert!(second_section_start > 0);
        assert!(!chunks[second_section_start].overlap_eligible);
        assert_eq!(chunks[second_section_start].overlap_tokens, 0);
        assert!(
            chunks
                .iter()
                .all(|chunk| encoded_len(&tokenizer, &chunk.content).is_ok_and(|len| len <= 12))
        );
        Ok(())
    }

    #[test]
    fn preserves_overlap_for_hard_token_boundary_splits() -> Result<()> {
        let tokenizer = test_tokenizer()?;
        let document = Document {
            path: "code.md".into(),
            content: format!("```text\n{}\n```", vec!["word"; 40].join(" ")),
            content_hash: String::new(),
        };

        let chunks = chunk(&document, &tokenizer, 12, 3)?;

        assert!(chunks.len() > 2);
        assert!(
            chunks
                .iter()
                .skip(1)
                .all(|chunk| { chunk.overlap_eligible && chunk.overlap_tokens == 3 })
        );
        assert!(
            chunks
                .iter()
                .all(|chunk| encoded_len(&tokenizer, &chunk.content).is_ok_and(|len| len <= 12))
        );
        Ok(())
    }

    #[test]
    fn supports_overlap_near_the_maximum_budget() -> Result<()> {
        let tokenizer = test_tokenizer()?;
        let document = document_with_words(15);

        let chunks = chunk(&document, &tokenizer, 12, 9)?;

        assert!(chunks.len() > 2);
        assert!(
            chunks
                .iter()
                .skip(1)
                .all(|chunk| { chunk.overlap_eligible && chunk.overlap_tokens == 9 })
        );
        assert!(
            chunks
                .iter()
                .all(|chunk| encoded_len(&tokenizer, &chunk.content).is_ok_and(|len| len <= 12))
        );
        Ok(())
    }

    #[test]
    fn splits_content_that_exceeds_512_tokens() -> Result<()> {
        let tokenizer = test_tokenizer()?;
        let document = document_with_words(511);

        assert_eq!(encoded_len(&tokenizer, &document.content)?, 513);
        let chunks = chunk(&document, &tokenizer, 512, 0)?;

        assert_eq!(chunks.len(), 2);
        assert!(
            chunks
                .iter()
                .all(|chunk| encoded_len(&tokenizer, &chunk.content).is_ok_and(|len| len <= 512))
        );
        assert_eq!(
            chunks
                .iter()
                .map(|chunk| chunk.content.as_str())
                .collect::<String>(),
            document.content
        );
        Ok(())
    }

    #[test]
    fn splits_a_multi_thousand_token_document_without_losing_text() -> Result<()> {
        let tokenizer = test_tokenizer()?;
        let document = document_with_words(2_000);

        let chunks = chunk(&document, &tokenizer, 512, 0)?;

        assert!(chunks.len() >= 4);
        assert!(
            chunks
                .iter()
                .all(|chunk| encoded_len(&tokenizer, &chunk.content).is_ok_and(|len| len <= 512))
        );
        assert_eq!(
            chunks
                .iter()
                .map(|chunk| chunk.content.as_str())
                .collect::<String>(),
            document.content
        );
        Ok(())
    }
}
