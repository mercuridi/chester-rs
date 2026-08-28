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
}
