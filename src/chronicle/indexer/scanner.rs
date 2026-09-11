// src/chronicle/indexer/scanner.rs

use std::{collections::HashSet, fs, path::Path};

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

#[derive(Debug, Clone)]
pub struct DocumentCandidate {
    pub path: std::path::PathBuf,
    pub metadata: super::frontmatter::Metadata,
    pub content_hash: String,
}

pub fn discover_directory_with_stats(
    root: impl AsRef<Path>,
) -> Result<(Vec<DocumentCandidate>, CorpusStats)> {
    discover_directory_with_stats_excluding(root, &HashSet::new())
}

pub fn discover_directory_with_stats_excluding(
    root: impl AsRef<Path>,
    excluded_note_ids: &HashSet<String>,
) -> Result<(Vec<DocumentCandidate>, CorpusStats)> {
    scan_directory_internal(root, excluded_note_ids)
}

#[instrument(skip(root))]
pub fn scan_directory_with_stats(root: impl AsRef<Path>) -> Result<(Vec<Document>, CorpusStats)> {
    scan_directory_with_stats_excluding(root, &HashSet::new())
}

pub fn scan_directory_with_stats_excluding(
    root: impl AsRef<Path>,
    excluded_note_ids: &HashSet<String>,
) -> Result<(Vec<Document>, CorpusStats)> {
    let root = root.as_ref();
    let (candidates, stats) = discover_directory_with_stats_excluding(root, excluded_note_ids)?;
    let documents = candidates
        .iter()
        .map(load_document)
        .collect::<Result<Vec<_>>>()?;
    Ok((documents, stats))
}

fn scan_directory_internal(
    root: impl AsRef<Path>,
    excluded_note_ids: &HashSet<String>,
) -> Result<(Vec<DocumentCandidate>, CorpusStats)> {
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
    if !is_templates_directory(root) {
        scan_directory_recursive_candidates(root, &mut documents, &mut stats, excluded_note_ids)?;
    }

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

pub fn load_document(candidate: &DocumentCandidate) -> Result<Document> {
    let document = scan_file(&candidate.path)?.ok_or_else(|| {
        anyhow::anyhow!(
            "Document became ineligible during indexing: {}",
            candidate.path.display()
        )
    })?;
    if document.content_hash != candidate.content_hash {
        anyhow::bail!(
            "Document changed while indexing: {}",
            candidate.path.display()
        );
    }
    Ok(document)
}

fn scan_directory_recursive_candidates(
    directory: &Path,
    documents: &mut Vec<DocumentCandidate>,
    stats: &mut CorpusStats,
    excluded_note_ids: &HashSet<String>,
) -> Result<()> {
    for entry in fs::read_dir(directory)
        .with_context(|| format!("failed to read directory: {}", directory.display()))?
    {
        let entry = entry.with_context(|| {
            format!("failed to read directory entry in {}", directory.display())
        })?;

        let path = entry.path();

        if path.is_dir() {
            // Templates are source material for note creation, not corpus
            // documents. Do not parse them: their intentionally incomplete
            // frontmatter must not block indexing the rest of the corpus.
            if is_templates_directory(&path) {
                continue;
            }
            stats.directories += 1;
            scan_directory_recursive_candidates(&path, documents, stats, excluded_note_ids)?;
            continue;
        }

        if !is_markdown_file(&path) {
            continue;
        }

        let Some(document) = scan_file(&path)? else {
            continue;
        };
        if excluded_note_ids.contains(&document.metadata.id) {
            continue;
        }
        stats.files += 1;
        stats.words += document.content.split_whitespace().count();
        stats.characters += document.content.chars().count();
        documents.push(document.candidate());
    }

    Ok(())
}

fn is_markdown_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
}

fn is_templates_directory(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            name.eq_ignore_ascii_case("templates") || name.eq_ignore_ascii_case("99 templates")
        })
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
    let (body, secret_callouts) = split_secret_callouts(&body, &metadata.visibility)
        .with_context(|| format!("Invalid secret callout in {}", path.display()))?;
    let content = format!(
        "# {title}\n\n{}\n\n{}\n\n{}\n\n{body}",
        metadata.aliases.join(", "),
        metadata.tags.join(", "),
        metadata.summary
    );
    Ok(Some(Document {
        metadata,
        path: path.to_path_buf(),
        content,
        public_body: body,
        secret_content: secret_callouts
            .iter()
            .map(|callout| format!("# {title}\n\n## {}\n\n{}", callout.title, callout.body))
            .collect(),
        secret_bodies: secret_callouts
            .into_iter()
            .map(|callout| callout.body)
            .collect(),
        content_hash,
    }))
}

#[derive(Debug)]
struct SecretCallout {
    title: String,
    body: String,
}

/// Separates Obsidian-style `[!secret]` callouts from player-visible Markdown.
/// A callout ends at the first line outside its block quote, as prescribed by
/// Markdown block quote syntax.
fn split_secret_callouts(body: &str, visibility: &str) -> Result<(String, Vec<SecretCallout>)> {
    let mut public = String::new();
    let mut secrets = Vec::new();
    let mut active = None::<(usize, String, String)>;

    for (line_index, line) in body.split_inclusive('\n').enumerate() {
        let line_number = line_index + 1;
        if let Some((start_line, _title, secret)) = active.as_mut() {
            if let Some(quoted) = quoted_callout_line(line) {
                if secret_callout_header(quoted)?.is_some() {
                    anyhow::bail!(
                        "nested [!secret] callout at body line {line_number} (opened at body line {start_line})"
                    );
                }
                secret.push_str(quoted);
                continue;
            }
            if let Some((_, title, secret)) = active.take() {
                secrets.push(SecretCallout {
                    title,
                    body: secret.trim().to_owned(),
                });
            }
        }

        if let Some(quoted) = quoted_callout_line(line)
            && let Some(title) = secret_callout_header(quoted)?
        {
            if visibility != "mixed" {
                anyhow::bail!(
                    "[!secret] callout at body line {line_number} requires visibility: mixed (found visibility: {visibility})"
                );
            }
            active = Some((line_number, title, String::new()));
            continue;
        }
        public.push_str(line);
    }

    if let Some((_, title, secret)) = active {
        secrets.push(SecretCallout {
            title,
            body: secret.trim().to_owned(),
        });
    }
    Ok((public, secrets))
}

fn quoted_callout_line(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let quoted = trimmed.strip_prefix('>')?;
    Some(quoted.strip_prefix(' ').unwrap_or(quoted))
}

/// Returns the callout title when this is a secret header. Non-secret callouts
/// are ordinary Markdown. A malformed secret header is an ingestion error.
fn secret_callout_header(quoted: &str) -> Result<Option<String>> {
    let trimmed = quoted.trim_end_matches(['\r', '\n']);
    let Some(after_marker) = trimmed.strip_prefix("[!secret]") else {
        return Ok(None);
    };
    if !after_marker.is_empty()
        && !after_marker.starts_with('-')
        && !after_marker.starts_with('+')
        && !after_marker.starts_with(char::is_whitespace)
    {
        anyhow::bail!("malformed [!secret] callout header");
    }
    let rest = after_marker
        .strip_prefix('-')
        .or_else(|| after_marker.strip_prefix('+'))
        .unwrap_or(after_marker)
        .trim();
    Ok(Some(if rest.is_empty() {
        "Secret".into()
    } else {
        rest.into()
    }))
}

fn hash_content(content: &str) -> String {
    let hash = Sha256::digest(content.as_bytes());
    hex::encode(hash)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{
        hash_content, is_markdown_file, is_templates_directory, scan_directory_with_stats,
        scan_directory_with_stats_excluding, split_secret_callouts,
    };
    use std::{collections::HashSet, fs, path::Path};
    use tempfile::tempdir;

    fn note(id: &str, status: &str) -> String {
        format!(
            "---\nid: {id}\ntype: location\nstatus: {status}\nvisibility: secret\ncreated: 2026-09-07\nupdated: 2026-09-07\naliases: [Moonspire]\ntags: [sanctuary, tower]\nsummary: A sanctuary\n---\nThe tower stands here."
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
        assert!(content.contains("sanctuary, tower"));
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
    fn excludes_configured_note_ids_before_returning_documents() -> anyhow::Result<()> {
        let directory = tempdir()?;
        fs::write(directory.path().join("index.md"), note("index", "canon"))?;
        fs::write(directory.path().join("real.md"), note("real", "canon"))?;
        let excluded = HashSet::from(["index".to_owned()]);

        let (documents, stats) = scan_directory_with_stats_excluding(directory.path(), &excluded)?;

        assert_eq!(stats.files, 1);
        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0].metadata.id, "real");
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
    fn recognises_templates_directories_case_insensitively() {
        assert!(is_templates_directory(Path::new("templates")));
        assert!(is_templates_directory(Path::new("Notes/Templates")));
        assert!(is_templates_directory(Path::new("TEMPLATES")));
        assert!(!is_templates_directory(Path::new("template")));
    }

    #[test]
    fn hashes_are_stable_and_content_sensitive() {
        assert_eq!(hash_content("same"), hash_content("same"));
        assert_ne!(hash_content("same"), hash_content("different"));
        assert_eq!(hash_content("").len(), 64);
    }

    #[test]
    fn separates_secret_callouts_from_player_content() -> anyhow::Result<()> {
        let (public, secrets) = split_secret_callouts(
            "Known history.\n\n> [!secret]- GM notes\n> The hidden name is Ilyra.\n> Keep this private.\n\nPublic aftermath.\n",
            "mixed",
        )?;
        assert!(public.contains("Known history."));
        assert!(public.contains("Public aftermath."));
        assert!(!public.contains("hidden name"));
        assert_eq!(secrets.len(), 1);
        assert_eq!(secrets[0].title, "GM notes");
        assert!(secrets[0].body.contains("hidden name"));
        Ok(())
    }

    #[test]
    fn rejects_secret_callouts_outside_mixed_notes_and_when_nested() {
        let callout = "> [!secret] GM\n> hidden\n";
        let error = split_secret_callouts(callout, "player").unwrap_err();
        assert!(error.to_string().contains("visibility: mixed"));

        let nested = "> [!secret] Outer\n> visible only to GMs\n> [!secret] Inner\n";
        let error = split_secret_callouts(nested, "mixed").unwrap_err();
        assert!(error.to_string().contains("nested"));
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
    fn skips_template_directories_before_frontmatter_validation() -> anyhow::Result<()> {
        let directory = tempdir()?;
        let templates = directory.path().join("templates");
        fs::create_dir(&templates)?;
        fs::write(
            directory.path().join("indexed.md"),
            note("indexed", "canon"),
        )?;
        fs::write(templates.join("unfinished.md"), "---\ntype: event\n")?;

        let (documents, stats) = scan_directory_with_stats(directory.path())?;

        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0].metadata.id, "indexed");
        assert_eq!(stats.directories, 1);
        assert_eq!(stats.files, 1);
        Ok(())
    }

    #[test]
    fn skips_a_templates_directory_used_as_the_scan_root() -> anyhow::Result<()> {
        let directory = tempdir()?;
        let templates = directory.path().join("templates");
        fs::create_dir(&templates)?;
        fs::write(templates.join("unfinished.md"), "---\ntype: event\n")?;
        let (documents, stats) = scan_directory_with_stats(&templates)?;
        assert!(documents.is_empty());
        assert_eq!(stats.directories, 1);
        assert_eq!(stats.files, 0);
        Ok(())
    }

    #[test]
    fn does_not_return_template_notes_for_embedding() -> anyhow::Result<()> {
        let directory = tempdir()?;
        let template = note("template", "canon").replace("type: location", "type: template");
        fs::write(directory.path().join("template.md"), template)?;

        let (documents, stats) = scan_directory_with_stats(directory.path())?;

        assert!(documents.is_empty());
        assert_eq!(stats.files, 0);
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
