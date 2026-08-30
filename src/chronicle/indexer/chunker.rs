use std::ops::Range;

use anyhow::{Result, anyhow};
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag};
use tokenizers::Tokenizer;

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

/// Split Markdown into token-bounded chunks while retaining the original source text.
pub fn chunk(
    document: &Document,
    tokenizer: &Tokenizer,
    max_tokens: usize,
    overlap_tokens: usize,
) -> Result<Vec<Chunk>> {
    if max_tokens < 3 {
        return Err(anyhow!("Chunk token budget must be at least 3"));
    }
    if overlap_tokens > max_tokens.saturating_sub(3) {
        return Err(anyhow!(
            "Chunk overlap must be no greater than the chunk budget minus 3"
        ));
    }

    if document.content.trim().is_empty() {
        return Ok(Vec::new());
    }

    let blocks = parse_blocks(&document.content);
    let mut chunks = Vec::new();
    let mut current = None::<(usize, usize, usize, BlockKind, Option<String>)>;

    for block in blocks {
        if matches!(block.kind, BlockKind::Heading(_))
            && let Some(chunk) = current.take()
        {
            chunks.push(chunk);
        }

        let candidate_start = current
            .as_ref()
            .map_or(block.range.start, |(start, _, _, _, _)| *start);
        let candidate = &document.content[candidate_start..block.range.end];

        if encoded_len(tokenizer, candidate)? <= max_tokens {
            current = Some((
                candidate_start,
                block.range.end,
                block.section,
                block.kind,
                block.heading,
            ));
            continue;
        }

        if let Some((start, end, section, kind, heading)) = current.take() {
            chunks.push((start, end, section, kind, heading));
        }

        let text = &document.content[block.range.clone()];
        if encoded_len(tokenizer, text)? <= max_tokens {
            current = Some((
                block.range.start,
                block.range.end,
                block.section,
                block.kind,
                block.heading,
            ));
        } else {
            let split = match block.kind {
                BlockKind::Paragraph | BlockKind::BlockQuote | BlockKind::ListItem => {
                    split_semantic_text(text, tokenizer, max_tokens)?
                }
                BlockKind::Heading(_) | BlockKind::CodeBlock | BlockKind::Table => {
                    split_long_text(text, tokenizer, max_tokens)?
                }
            };
            chunks.extend(split.into_iter().map(|range| {
                (
                    block.range.start + range.start,
                    block.range.start + range.end,
                    block.section,
                    block.kind,
                    block.heading.clone(),
                )
            }));
        }
    }

    if let Some(chunk) = current {
        chunks.push(chunk);
    }

    apply_overlap(
        &mut chunks,
        &document.content,
        tokenizer,
        max_tokens,
        overlap_tokens,
    )?;

    Ok(chunks
        .into_iter()
        .enumerate()
        .map(|(index, (start, end, _, _, heading))| Chunk {
            document_path: document.path.clone(),
            index,
            content: document.content[start..end].to_owned(),
            heading,
        })
        .collect())
}

fn apply_overlap(
    chunks: &mut [(usize, usize, usize, BlockKind, Option<String>)],
    source: &str,
    tokenizer: &Tokenizer,
    max_tokens: usize,
    overlap_tokens: usize,
) -> Result<()> {
    if overlap_tokens == 0 {
        return Ok(());
    }

    for index in 1..chunks.len() {
        let (previous, current) = chunks.split_at_mut(index);
        let (previous_start, previous_end, previous_section, previous_kind, _) =
            &previous[index - 1];
        let (current_start, current_end, current_section, _, _) = &current[0];

        if previous_section != current_section || matches!(previous_kind, BlockKind::Heading(_)) {
            continue;
        }

        let previous_text = &source[*previous_start..*previous_end];
        let mut candidates =
            overlap_candidates(previous_text, *previous_kind, tokenizer, overlap_tokens)?;
        candidates.sort_by_key(|(_, token_count)| std::cmp::Reverse(*token_count));

        for (local_start, _) in candidates {
            let overlap_start = *previous_start + local_start;
            if overlap_start >= *current_start
                || encoded_len(tokenizer, &source[overlap_start..*current_end])? > max_tokens
            {
                continue;
            }

            current[0].0 = overlap_start;
            break;
        }
    }

    Ok(())
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
        let token_count = encoded_len(tokenizer, &text[start..])?;
        if token_count <= overlap_tokens {
            candidates.push((start, token_count));
        }
    }

    Ok(candidates)
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

                if let Some(kind) = block_kind(&tag)
                    && open_blocks.is_empty()
                {
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
            Event::Rule => blocks.push(ParsedBlock {
                range,
                kind: BlockKind::Paragraph,
                section,
                heading: Some(format_heading_path(&heading_path)).filter(|path| !path.is_empty()),
            }),
            _ => {}
        }
    }

    blocks.sort_by_key(|block| block.range.start);
    blocks
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
    let mut ranges = Vec::new();
    let mut start = 0;

    for (index, character) in text.char_indices() {
        if !matches!(character, '.' | '!' | '?') {
            continue;
        }

        let end = index + character.len_utf8();
        if text[end..].chars().next().is_some_and(char::is_whitespace) {
            ranges.push(start..end);
            start = end;
        }
    }

    if start < text.len() {
        ranges.push(start..text.len());
    }
    ranges
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

    #[test]
    fn parses_markdown_blocks_and_heading_hierarchy() {
        let source = "# Top\n\nIntro [link](https://example.com) ![image](image.png)\n\n## Nested\n\n- one\n- two\n\n> quoted\n\n| Name | Value |\n| --- | --- |\n| A | B |\n\n---\n\n```rust\nlet answer = 42;\n```\n";

        let blocks = parse_blocks(source);

        assert_eq!(blocks.len(), 9);
        assert_eq!(&source[blocks[0].range.clone()], "# Top\n");
        assert_eq!(blocks[0].heading.as_deref(), Some("Top"));
        assert_eq!(
            &source[blocks[1].range.clone()],
            "Intro [link](https://example.com) ![image](image.png)\n"
        );
        assert_eq!(blocks[1].heading.as_deref(), Some("Top"));
        assert_eq!(&source[blocks[2].range.clone()], "## Nested\n");
        assert_eq!(blocks[2].heading.as_deref(), Some("Top > Nested"));
        assert!(source[blocks[3].range.clone()].starts_with("- one"));
        assert!(source[blocks[4].range.clone()].starts_with("- two"));
        assert!(source[blocks[5].range.clone()].starts_with("> quoted"));
        assert!(source[blocks[6].range.clone()].starts_with("| Name | Value |"));
        assert_eq!(&source[blocks[7].range.clone()], "---\n");
        assert!(source[blocks[8].range.clone()].starts_with("```rust\n"));
        assert_eq!(blocks[8].heading.as_deref(), Some("Top > Nested"));
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
