// src/chronicle/indexer/scanner.rs

use std::{fs, path::Path};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use tracing::{info, instrument};

use crate::chronicle::indexer::document::Document;

#[derive(Debug, Default, Clone, Copy)]
pub struct CorpusStats {
    pub directory_count: usize,
    pub file_count: usize,
    pub word_count: usize,
    pub character_count: usize,
}

#[instrument(skip(root))]
pub fn scan_directory_with_stats(root: impl AsRef<Path>) -> Result<(Vec<Document>, CorpusStats)> {
    let root = root.as_ref();

    if !root.is_dir() {
        anyhow::bail!(
            "index directory does not exist or is not a directory: {}",
            root.display()
        );
    }

    let mut documents = Vec::new();
    let mut stats = CorpusStats {
        directory_count: 1,
        ..CorpusStats::default()
    };
    scan_directory_recursive(root, &mut documents, &mut stats)?;

    documents.sort_by(|a, b| a.path.cmp(&b.path));
    let average_words_per_file = if stats.file_count == 0 {
        0.0
    } else {
        stats.word_count as f64 / stats.file_count as f64
    };
    info!(
        directory_count = stats.directory_count,
        file_count = stats.file_count,
        word_count = stats.word_count,
        character_count = stats.character_count,
        average_words_per_file,
        "Scanned Chronicle corpus"
    );

    Ok((documents, stats))
}

fn scan_directory_recursive(
    directory: &Path,
    documents: &mut Vec<Document>,
    stats: &mut CorpusStats,
) -> Result<()> {
    for entry in fs::read_dir(directory)
        .with_context(|| format!("failed to read directory: {}", directory.display()))?
    {
        let entry = entry.with_context(|| {
            format!("failed to read directory entry in {}", directory.display())
        })?;

        let path = entry.path();

        if path.is_dir() {
            stats.directory_count += 1;
            scan_directory_recursive(&path, documents, stats)?;
            continue;
        }

        if !is_markdown_file(&path) {
            continue;
        }

        let document = scan_file(&path)?;
        stats.file_count += 1;
        stats.word_count += document.content.split_whitespace().count();
        stats.character_count += document.content.chars().count();
        documents.push(document);
    }

    Ok(())
}

fn is_markdown_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
}

fn scan_file(path: &Path) -> Result<Document> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;

    let content_hash = hash_content(&content);

    Ok(Document {
        path: path.to_path_buf(),
        content,
        content_hash,
    })
}

fn hash_content(content: &str) -> String {
    let hash = Sha256::digest(content.as_bytes());
    hex::encode(hash)
}
