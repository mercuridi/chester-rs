use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkVisibility {
    Player,
    Secret,
}

impl ChunkVisibility {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Player => "player",
            Self::Secret => "secret",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Document {
    pub metadata: super::frontmatter::Metadata,
    pub path: PathBuf,
    pub content: String,
    /// Individually protected passages extracted from `[!secret]` callouts.
    pub secret_content: Vec<String>,
    pub content_hash: String,
}

#[derive(Debug, Clone)]
pub struct Chunk {
    #[expect(dead_code, reason = "Retained for future chunk provenance")]
    pub document_path: PathBuf,
    pub index: usize,
    pub content: String,
    pub visibility: ChunkVisibility,
    pub heading: Option<String>,
    /// Whether this chunk follows an overlap-compatible chunk in the same Markdown section.
    pub overlap_eligible: bool,
    /// Actual content tokens shared with the preceding chunk.
    pub overlap_tokens: usize,
}
