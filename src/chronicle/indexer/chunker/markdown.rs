use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag};

use super::types::{BlockKind, ParsedBlock};

pub(super) fn parse_blocks(source: &str) -> Vec<ParsedBlock> {
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
