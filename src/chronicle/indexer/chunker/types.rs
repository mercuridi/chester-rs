use std::ops::Range;

use pulldown_cmark::HeadingLevel;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BlockKind {
    Paragraph,
    Heading(HeadingLevel),
    BlockQuote,
    CodeBlock,
    ListItem,
    Table,
}

#[derive(Debug)]
pub(super) struct ParsedBlock {
    pub(super) range: Range<usize>,
    pub(super) kind: BlockKind,
    pub(super) section: usize,
    pub(super) heading: Option<String>,
}

#[derive(Debug)]
pub(super) struct PlannedChunk {
    pub(super) range: Range<usize>,
    pub(super) kind: BlockKind,
    pub(super) section: usize,
    pub(super) heading: Option<String>,
    pub(super) token_budget: usize,
}

#[derive(Debug, Default, Clone, Copy)]
pub(super) struct OverlapInfo {
    pub(super) eligible: bool,
    pub(super) tokens: usize,
}
