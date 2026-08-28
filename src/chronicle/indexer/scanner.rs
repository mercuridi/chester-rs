// src/chronicle/indexer/scanner.rs

use std::{fs, path::Path};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use tracing::{info, instrument};

use crate::chronicle::indexer::document::Document;

#[instrument(skip(root))]
pub fn scan_directory(root: impl AsRef<Path>) -> Result<Vec<Document>> {
    let root = root.as_ref();

    if !root.is_dir() {
        anyhow::bail!(
            "index directory does not exist or is not a directory: {}",
            root.display()
        );
    }

    let mut documents = Vec::new();
    scan_directory_recursive(root, &mut documents)?;

    documents.sort_by(|a, b| a.path.cmp(&b.path));
    info!(document_count = documents.len(), "Scanned Chronicle corpus");

    Ok(documents)
}

fn scan_directory_recursive(directory: &Path, documents: &mut Vec<Document>) -> Result<()> {
    for entry in fs::read_dir(directory)
        .with_context(|| format!("failed to read directory: {}", directory.display()))?
    {
        let entry = entry.with_context(|| {
            format!("failed to read directory entry in {}", directory.display())
        })?;

        let path = entry.path();

        if path.is_dir() {
            scan_directory_recursive(&path, documents)?;
            continue;
        }

        if !is_markdown_file(&path) {
            continue;
        }

        documents.push(scan_file(&path)?);
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
