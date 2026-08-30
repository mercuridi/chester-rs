use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Document {
    pub path: PathBuf,
    pub content: String,
    pub content_hash: String,
}

#[derive(Debug, Clone)]
pub struct Chunk {
    #[expect(dead_code, reason = "Retained for future chunk provenance")]
    pub document_path: PathBuf,
    pub index: usize,
    pub content: String,
    pub heading: Option<String>,
    /// Whether this chunk follows an overlap-compatible chunk in the same Markdown section.
    pub overlap_eligible: bool,
    /// Actual content tokens shared with the preceding chunk.
    pub overlap_tokens: usize,
}
