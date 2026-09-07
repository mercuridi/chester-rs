// src/chronicle/indexer/scanner.rs

use std::{fs, path::Path};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use tracing::{info, instrument};

use crate::chronicle::indexer::document::Document;

#[derive(Debug, Default, Clone, Copy)]
pub struct CorpusStats {
    pub directories: usize,
    pub files: usize,
    pub words: usize,
    pub characters: usize,
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
        directories: 1,
        ..CorpusStats::default()
    };
    scan_directory_recursive(root, &mut documents, &mut stats)?;

    let mut ids = std::collections::HashSet::new();
    for document in &documents {
        if !ids.insert(&document.metadata.id) {
            anyhow::bail!("Duplicate Chronicle note ID: {}", document.metadata.id);
        }
    }
    documents.sort_by(|a, b| a.path.cmp(&b.path));
    #[allow(clippy::cast_precision_loss)]
    let average_words_per_file = if stats.files == 0 {
        0.0
    } else {
        stats.words as f64 / stats.files as f64
    };
    info!(
        directory_count = stats.directories,
        file_count = stats.files,
        word_count = stats.words,
        character_count = stats.characters,
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
            stats.directories += 1;
            scan_directory_recursive(&path, documents, stats)?;
            continue;
        }

        if !is_markdown_file(&path) {
            continue;
        }

        let Some(document) = scan_file(&path)? else {
            continue;
        };
        stats.files += 1;
        stats.words += document.content.split_whitespace().count();
        stats.characters += document.content.chars().count();
        documents.push(document);
    }

    Ok(())
}

fn is_markdown_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
}

fn scan_file(path: &Path) -> Result<Option<Document>> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;

    let content_hash = hash_content(&content);

    let Some((metadata, body)) = super::frontmatter::parse(&content)
        .with_context(|| format!("Invalid note {}", path.display()))?
    else {
        return Ok(None);
    };
    if metadata.status != "canon" || metadata.note_type == "template" {
        return Ok(None);
    }
    let title = path.file_stem().unwrap_or_default().to_string_lossy();
    let content = format!(
        "# {title}\n\n{}\n\n{}\n\n{body}",
        metadata.aliases.join(", "),
        metadata.summary
    );
    Ok(Some(Document {
        metadata,
        path: path.to_path_buf(),
        content,
        content_hash,
    }))
}

fn hash_content(content: &str) -> String {
    let hash = Sha256::digest(content.as_bytes());
    hex::encode(hash)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{hash_content, is_markdown_file, scan_directory_with_stats};
    use std::{fs, path::Path};
    use tempfile::tempdir;

    fn note(id: &str, status: &str) -> String {
        format!(
            "---\nid: {id}\ntype: location\nstatus: {status}\nvisibility: secret\ncreated: 2026-09-07\nupdated: 2026-09-07\naliases: [Moonspire]\nsummary: A sanctuary\n---\nThe tower stands here."
        )
    }

    #[test]
    fn indexes_clean_canon_and_rejects_duplicate_ids() -> anyhow::Result<()> {
        let directory = tempdir()?;
        fs::write(directory.path().join("tower.md"), note("tower", "canon"))?;
        fs::write(directory.path().join("draft.md"), note("draft", "draft"))?;
        let (documents, _) = scan_directory_with_stats(directory.path())?;
        assert_eq!(documents.len(), 1);
        let content = &documents[0].content;
        assert!(content.contains("Moonspire"));
        assert!(content.contains("A sanctuary"));
        assert!(!content.contains("updated"));
        assert!(!content.contains("visibility"));
        fs::write(
            directory.path().join("duplicate.md"),
            note("tower", "canon"),
        )?;
        assert!(scan_directory_with_stats(directory.path()).is_err());
        Ok(())
    }

    #[test]
    fn recognises_markdown_extensions_case_insensitively() {
        assert!(is_markdown_file(Path::new("notes.md")));
        assert!(is_markdown_file(Path::new("notes.MD")));
        assert!(!is_markdown_file(Path::new("notes.txt")));
        assert!(!is_markdown_file(Path::new("notes")));
    }

    #[test]
    fn hashes_are_stable_and_content_sensitive() {
        assert_eq!(hash_content("same"), hash_content("same"));
        assert_ne!(hash_content("same"), hash_content("different"));
        assert_eq!(hash_content("").len(), 64);
    }

    #[test]
    fn scans_nested_markdown_and_ignores_other_files() -> anyhow::Result<()> {
        let directory = tempdir()?;
        let nested = directory.path().join("nested");
        fs::create_dir(&nested)?;
        fs::write(directory.path().join("b.md"), note("b", "canon"))?;
        fs::write(nested.join("a.MD"), note("a", "canon"))?;
        fs::write(nested.join("ignored.txt"), "not counted")?;

        let (documents, stats) = scan_directory_with_stats(directory.path())?;

        assert_eq!(stats.directories, 2);
        assert_eq!(stats.files, 2);
        assert!(stats.words > 0);
        assert!(stats.characters > 0);
        assert!(documents[0].path < documents[1].path);
        assert!(
            documents
                .iter()
                .all(|document| document.content_hash.len() == 64)
        );
        Ok(())
    }

    #[test]
    fn empty_directory_returns_empty_documents_and_root_stat() -> anyhow::Result<()> {
        let directory = tempdir()?;
        let (documents, stats) = scan_directory_with_stats(directory.path())?;
        assert!(documents.is_empty());
        assert_eq!(stats.directories, 1);
        assert_eq!(stats.files, 0);
        assert_eq!(stats.words, 0);
        assert_eq!(stats.characters, 0);
        Ok(())
    }

    #[test]
    fn rejects_missing_paths_and_regular_files() -> anyhow::Result<()> {
        let directory = tempdir()?;
        let file = directory.path().join("file.md");
        fs::write(&file, "content")?;
        assert!(
            scan_directory_with_stats(&file)
                .unwrap_err()
                .to_string()
                .contains("not a directory")
        );
        assert!(
            scan_directory_with_stats(directory.path().join("missing"))
                .unwrap_err()
                .to_string()
                .contains("not a directory")
        );
        Ok(())
    }
}
