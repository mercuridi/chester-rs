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
    List,
    Table,
}

#[derive(Debug)]
struct ParsedBlock {
    range: Range<usize>,
    heading: Option<String>,
}

/// Split Markdown into token-bounded chunks while retaining the original source text.
pub fn chunk(document: &Document, tokenizer: &Tokenizer, max_tokens: usize) -> Result<Vec<Chunk>> {
    if max_tokens < 3 {
        return Err(anyhow!("Chunk token budget must be at least 3"));
    }

    if document.content.trim().is_empty() {
        return Ok(Vec::new());
    }

    let blocks = parse_blocks(&document.content);
    let mut chunks = Vec::new();
    let mut current = None::<(usize, usize, Option<String>)>;

    for block in blocks {
        let candidate_start = current
            .as_ref()
            .map_or(block.range.start, |(start, _, _)| *start);
        let candidate = &document.content[candidate_start..block.range.end];

        if encoded_len(tokenizer, candidate)? <= max_tokens {
            current = Some((candidate_start, block.range.end, block.heading));
            continue;
        }

        if let Some((start, end, heading)) = current.take() {
            chunks.push((start, end, heading));
        }

        let text = &document.content[block.range.clone()];
        if encoded_len(tokenizer, text)? <= max_tokens {
            current = Some((block.range.start, block.range.end, block.heading));
        } else {
            chunks.extend(
                split_long_text(text, tokenizer, max_tokens)?
                    .into_iter()
                    .map(|range| {
                        (
                            block.range.start + range.start,
                            block.range.start + range.end,
                            block.heading.clone(),
                        )
                    }),
            );
        }
    }

    if let Some(chunk) = current {
        chunks.push(chunk);
    }

    Ok(chunks
        .into_iter()
        .enumerate()
        .map(|(index, (start, end, heading))| Chunk {
            document_path: document.path.clone(),
            index,
            content: document.content[start..end].to_owned(),
            heading,
        })
        .collect())
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
        Tag::List(_) => Some(BlockKind::List),
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

    #[test]
    fn parses_markdown_blocks_and_heading_hierarchy() {
        let source = "# Top\n\nIntro [link](https://example.com) ![image](image.png)\n\n## Nested\n\n- one\n- two\n\n> quoted\n\n| Name | Value |\n| --- | --- |\n| A | B |\n\n---\n\n```rust\nlet answer = 42;\n```\n";

        let blocks = parse_blocks(source);

        assert_eq!(blocks.len(), 8);
        assert_eq!(&source[blocks[0].range.clone()], "# Top\n");
        assert_eq!(blocks[0].heading.as_deref(), Some("Top"));
        assert_eq!(
            &source[blocks[1].range.clone()],
            "Intro [link](https://example.com) ![image](image.png)\n"
        );
        assert_eq!(blocks[1].heading.as_deref(), Some("Top"));
        assert_eq!(&source[blocks[2].range.clone()], "## Nested\n");
        assert_eq!(blocks[2].heading.as_deref(), Some("Top > Nested"));
        assert!(source[blocks[3].range.clone()].starts_with("- one\n- two"));
        assert!(source[blocks[4].range.clone()].starts_with("> quoted"));
        assert!(source[blocks[5].range.clone()].starts_with("| Name | Value |"));
        assert_eq!(&source[blocks[6].range.clone()], "---\n");
        assert!(source[blocks[7].range.clone()].starts_with("```rust\n"));
        assert_eq!(blocks[7].heading.as_deref(), Some("Top > Nested"));
    }
}
