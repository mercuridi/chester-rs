// src/chronicle/indexer/scanner.rs

use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use tracing::{info, instrument};

use crate::chronicle::indexer::Document;

#[derive(Debug, Default, Clone, Copy)]
pub struct CorpusStats {
    pub directories: usize,
    pub files: usize,
    pub words: usize,
    pub characters: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Used by the resilient scan implementation in phase 2.
pub(crate) enum CorpusErrorKind {
    Read,
    DirectoryTraversal,
    Frontmatter,
    SecretCallout,
    DuplicateId,
    Resolution,
}

impl CorpusErrorKind {
    fn order(self) -> usize {
        match self {
            Self::Read => 0,
            Self::DirectoryTraversal => 1,
            Self::Frontmatter => 2,
            Self::SecretCallout => 3,
            Self::DuplicateId => 4,
            Self::Resolution => 5,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::DirectoryTraversal => "directory-traversal",
            Self::Frontmatter => "frontmatter",
            Self::SecretCallout => "secret-callout",
            Self::DuplicateId => "duplicate-id",
            Self::Resolution => "resolution",
        }
    }
}

#[derive(Debug)]
#[allow(dead_code)] // Used by the resilient scan implementation in phase 2.
pub(crate) struct CorpusDiagnostic {
    pub(crate) path: Option<PathBuf>,
    pub(crate) kind: CorpusErrorKind,
    pub(crate) error: anyhow::Error,
}

#[derive(Debug)]
#[allow(dead_code)] // Used by the resilient scan implementation in phase 2.
pub(crate) struct CorpusErrors {
    root: PathBuf,
    pub(crate) diagnostics: Vec<CorpusDiagnostic>,
}

#[allow(dead_code)] // Used by the resilient scan implementation in phase 2.
impl CorpusErrors {
    pub(crate) fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            diagnostics: Vec::new(),
        }
    }

    pub(crate) fn push(
        &mut self,
        path: Option<PathBuf>,
        kind: CorpusErrorKind,
        error: anyhow::Error,
    ) {
        self.diagnostics
            .push(CorpusDiagnostic { path, kind, error });
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.diagnostics.is_empty()
    }

    pub(crate) fn into_error(mut self) -> anyhow::Error {
        self.sort();
        anyhow::Error::new(self)
    }

    fn sort(&mut self) {
        self.diagnostics.sort_by(|left, right| {
            let left_path = left
                .path
                .as_deref()
                .map_or_else(String::new, |path| path.to_string_lossy().into_owned());
            let right_path = right
                .path
                .as_deref()
                .map_or_else(String::new, |path| path.to_string_lossy().into_owned());
            left_path
                .cmp(&right_path)
                .then_with(|| left.kind.order().cmp(&right.kind.order()))
                .then_with(|| format!("{:#}", left.error).cmp(&format!("{:#}", right.error)))
        });
    }
}

impl std::fmt::Display for CorpusErrors {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            formatter,
            "{} corpus error(s) in {}:",
            self.diagnostics.len(),
            self.root.display()
        )?;
        for diagnostic in &self.diagnostics {
            let path = diagnostic
                .path
                .as_deref()
                .map_or_else(|| "<corpus>".to_owned(), |path| self.display_path(path));
            writeln!(
                formatter,
                "- [{}] {path}: {:#}",
                diagnostic.kind.label(),
                diagnostic.error
            )?;
        }
        Ok(())
    }
}

impl CorpusErrors {
    fn display_path(&self, path: &Path) -> String {
        display_corpus_path(&self.root, path)
    }
}

/// Formats a diagnostic path relative to the corpus root when possible.
///
/// Some diagnostics use labels rather than filesystem paths (for example a
/// duplicate note ID), so paths that are not below the root are preserved as
/// supplied. Forward slashes keep the user-facing report consistent across
/// platforms.
fn display_corpus_path(root: &Path, path: &Path) -> String {
    let path = path.strip_prefix(root).unwrap_or(path);
    if path.as_os_str().is_empty() {
        return ".".to_owned();
    }
    path.to_string_lossy().replace('\\', "/")
}

impl std::error::Error for CorpusErrors {}

#[derive(Debug)]
pub(crate) struct CorpusScan {
    pub(crate) documents: Vec<Document>,
    pub(crate) stats: CorpusStats,
    pub(crate) errors: CorpusErrors,
}

pub fn discover_directory_with_stats_excluding(
    root: impl AsRef<Path>,
    excluded_note_ids: &HashSet<String>,
) -> Result<(Vec<Document>, CorpusStats)> {
    let scan = scan_directory_internal(root, excluded_note_ids)?;
    if !scan.errors.is_empty() {
        return Err(scan.errors.into_error());
    }
    Ok((scan.documents, scan.stats))
}

pub(crate) fn scan_directory_partial_with_stats_excluding(
    root: impl AsRef<Path>,
    excluded_note_ids: &HashSet<String>,
) -> Result<CorpusScan> {
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
    discover_directory_with_stats_excluding(root, excluded_note_ids)
}

fn scan_directory_internal(
    root: impl AsRef<Path>,
    excluded_note_ids: &HashSet<String>,
) -> Result<CorpusScan> {
    let root = root.as_ref();

    if !root.is_dir() {
        anyhow::bail!(
            "index directory does not exist or is not a directory: {}",
            root.display()
        );
    }

    let mut documents: Vec<Document> = Vec::new();
    let mut stats = CorpusStats {
        directories: 1,
        ..CorpusStats::default()
    };
    let mut errors = CorpusErrors::new(root);
    if !is_templates_directory(root) {
        scan_directory_recursive_documents(
            root,
            root,
            &mut documents,
            &mut stats,
            &mut errors,
            excluded_note_ids,
        );
    }

    let mut paths_by_id = BTreeMap::<String, Vec<PathBuf>>::new();
    for document in &documents {
        paths_by_id
            .entry(document.metadata.id.clone())
            .or_default()
            .push(document.path.clone());
    }
    for (id, mut paths) in paths_by_id {
        if paths.len() < 2 {
            continue;
        }
        paths.sort();
        let conflicting_paths = paths
            .iter()
            .map(|path| format!("- {}", display_corpus_path(root, path)))
            .collect::<Vec<_>>()
            .join("\n");
        errors.push(
            Some(format!("`{id}`").into()),
            CorpusErrorKind::DuplicateId,
            anyhow::anyhow!("appears in:\n{conflicting_paths}"),
        );
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

    Ok(CorpusScan {
        documents,
        stats,
        errors,
    })
}

fn scan_directory_recursive_documents(
    root: &Path,
    directory: &Path,
    documents: &mut Vec<Document>,
    stats: &mut CorpusStats,
    errors: &mut CorpusErrors,
    excluded_note_ids: &HashSet<String>,
) {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) => {
            errors.push(
                Some(directory.to_path_buf()),
                CorpusErrorKind::DirectoryTraversal,
                anyhow::Error::from(error).context(format!(
                    "failed to read directory: {}",
                    display_corpus_path(root, directory)
                )),
            );
            return;
        }
    };

    let mut readable_entries = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                errors.push(
                    Some(directory.to_path_buf()),
                    CorpusErrorKind::DirectoryTraversal,
                    anyhow::Error::from(error).context(format!(
                        "failed to read directory entry in {}",
                        display_corpus_path(root, directory)
                    )),
                );
                continue;
            }
        };
        readable_entries.push(entry);
    }
    readable_entries.sort_by_key(std::fs::DirEntry::path);

    for entry in readable_entries {
        let path = entry.path();

        if path.is_dir() {
            // Templates are source material for note creation, not corpus
            // documents. Do not parse them: their intentionally incomplete
            // frontmatter must not block indexing the rest of the corpus.
            if is_templates_directory(&path) {
                continue;
            }
            stats.directories += 1;
            scan_directory_recursive_documents(
                root,
                &path,
                documents,
                stats,
                errors,
                excluded_note_ids,
            );
            continue;
        }

        if !is_markdown_file(&path) {
            continue;
        }

        let document = match scan_file(root, &path) {
            Ok(document) => document,
            Err(error) => {
                errors.push(Some(path.clone()), scan_error_kind(&error), error);
                continue;
            }
        };
        let Some(document) = document else {
            continue;
        };
        if excluded_note_ids.contains(&document.metadata.id) {
            continue;
        }
        stats.files += 1;
        stats.words += document.content.split_whitespace().count();
        stats.characters += document.content.chars().count();
        documents.push(document);
    }
}

fn scan_error_kind(error: &anyhow::Error) -> CorpusErrorKind {
    let message = error.to_string();
    if message.contains("Invalid secret callout") {
        CorpusErrorKind::SecretCallout
    } else if message.contains("Invalid note") {
        CorpusErrorKind::Frontmatter
    } else {
        CorpusErrorKind::Read
    }
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

fn scan_file(root: &Path, path: &Path) -> Result<Option<Document>> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", display_corpus_path(root, path)))?;

    let content_hash = hash_content(&content);

    let Some((metadata, body)) = super::frontmatter::parse(&content)
        .with_context(|| format!("Invalid note {}", display_corpus_path(root, path)))?
    else {
        return Ok(None);
    };
    if metadata.status != "canon" || metadata.note_type == "template" {
        return Ok(None);
    }
    let title = path.file_stem().unwrap_or_default().to_string_lossy();
    let (body, secret_callouts) =
        split_secret_callouts(&body, &metadata.visibility).with_context(|| {
            format!(
                "Invalid secret callout in {}",
                display_corpus_path(root, path)
            )
        })?;
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
        CorpusErrorKind, CorpusErrors, display_corpus_path, hash_content, is_markdown_file,
        is_templates_directory, scan_directory_with_stats, scan_directory_with_stats_excluding,
        split_secret_callouts,
    };
    use std::{collections::HashSet, fs, path::Path};
    use tempfile::tempdir;

    #[test]
    fn corpus_errors_are_sorted_by_path_then_category() {
        let mut errors = CorpusErrors::new("/corpus");
        errors.push(
            Some("z.md".into()),
            CorpusErrorKind::Read,
            anyhow::anyhow!("could not read"),
        );
        errors.push(
            Some("a.md".into()),
            CorpusErrorKind::SecretCallout,
            anyhow::anyhow!("invalid callout"),
        );
        errors.push(
            Some("a.md".into()),
            CorpusErrorKind::Frontmatter,
            anyhow::anyhow!("invalid metadata"),
        );

        let report = errors.into_error().to_string();
        let frontmatter = report.find("[frontmatter]").unwrap();
        let secret_callout = report.find("[secret-callout]").unwrap();
        let read = report.find("[read]").unwrap();

        assert!(frontmatter < secret_callout);
        assert!(secret_callout < read);
    }

    #[test]
    fn display_corpus_path_formats_relative_paths_and_preserves_other_labels() {
        let root = Path::new("/srv/chester/corpus");

        assert_eq!(
            display_corpus_path(root, Path::new("/srv/chester/corpus/characters/bad.md")),
            "characters/bad.md"
        );
        assert_eq!(display_corpus_path(root, root), ".");
        assert_eq!(
            display_corpus_path(root, Path::new("/elsewhere/bad.md")),
            "/elsewhere/bad.md"
        );
        assert_eq!(
            display_corpus_path(root, Path::new("`northmere`")),
            "`northmere`"
        );
        assert_eq!(
            display_corpus_path(root, Path::new("/srv/chester/corpus/regions\\bad.md")),
            "regions/bad.md"
        );
    }

    #[test]
    fn corpus_errors_display_all_entries_and_preserve_error_chains() {
        let error = anyhow::anyhow!("permission denied")
            .context("could not read note")
            .context("corpus access failed");
        let mut errors = CorpusErrors::new("/corpus");
        errors.push(Some("notes/one.md".into()), CorpusErrorKind::Read, error);
        errors.push(
            Some("northmere".into()),
            CorpusErrorKind::DuplicateId,
            anyhow::anyhow!("also declared by notes/two.md"),
        );

        let error = errors.into_error();
        let report = format!("{error:#}");

        assert!(report.contains("2 corpus error(s) in /corpus:"));
        assert!(report.contains(
            "[read] notes/one.md: corpus access failed: could not read note: permission denied"
        ));
        assert!(report.contains("[duplicate-id] northmere: also declared by notes/two.md"));
    }

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
    fn collects_all_duplicate_ids_and_conflicting_paths_in_order() -> anyhow::Result<()> {
        let directory = tempdir()?;
        fs::create_dir(directory.path().join("locations"))?;
        fs::create_dir(directory.path().join("regions"))?;
        fs::write(
            directory.path().join("locations/northmere.md"),
            note("northmere", "canon"),
        )?;
        fs::write(
            directory.path().join("regions/northmere-copy.md"),
            note("northmere", "canon"),
        )?;
        fs::write(
            directory.path().join("locations/ember.md"),
            note("ember", "canon"),
        )?;
        fs::write(
            directory.path().join("regions/ember-copy.md"),
            note("ember", "canon"),
        )?;

        let error = scan_directory_with_stats(directory.path()).unwrap_err();
        let report = format!("{error:#}");

        assert!(report.contains(&format!(
            "2 corpus error(s) in {}:",
            directory.path().display()
        )));
        assert_eq!(
            report
                .matches(&directory.path().display().to_string())
                .count(),
            1
        );
        assert!(
            report.contains("[duplicate-id] `ember`: appears in:"),
            "{report}"
        );
        assert!(
            report.contains("[duplicate-id] `northmere`: appears in:"),
            "{report}"
        );
        let ember_start = report.find("[duplicate-id] `ember`").unwrap();
        let ember_end = report.find("[duplicate-id] `northmere`").unwrap();
        let ember_report = &report[ember_start..ember_end];
        assert!(ember_report.contains("locations/ember.md"));
        assert!(ember_report.contains("regions/ember-copy.md"));
        assert!(report.contains("locations/northmere.md"));
        assert!(report.contains("regions/northmere-copy.md"));
        assert!(ember_start < ember_end);
        Ok(())
    }

    #[test]
    fn partial_scan_retains_valid_documents_and_collects_sibling_errors() -> anyhow::Result<()> {
        let directory = tempdir()?;
        fs::write(directory.path().join("valid.md"), note("valid", "canon"))?;
        fs::write(
            directory.path().join("bad-frontmatter.md"),
            "---\ntype: location\n---\nIncomplete note",
        )?;
        fs::write(
            directory.path().join("bad-secret.md"),
            format!(
                "{}\n\n> [!secret] GM notes\n> Hidden information.\n",
                note("bad-secret", "canon").replace("visibility: secret", "visibility: player")
            ),
        )?;

        let scan =
            super::scan_directory_partial_with_stats_excluding(directory.path(), &HashSet::new())?;

        assert_eq!(scan.documents.len(), 1);
        let document = &scan.documents[0];
        assert_eq!(document.metadata.id, "valid");
        assert!(document.content.contains("The tower stands here."));
        assert_eq!(document.public_body, "The tower stands here.");
        assert_eq!(document.content_hash.len(), 64);
        assert_eq!(scan.stats.files, 1);
        assert_eq!(scan.errors.diagnostics.len(), 2);
        assert!(scan.errors.diagnostics.iter().all(|diagnostic| {
            diagnostic
                .path
                .as_deref()
                .is_some_and(|path| path.starts_with(directory.path()))
        }));
        let report = scan.errors.to_string();
        assert_eq!(
            report
                .matches(&directory.path().display().to_string())
                .count(),
            1
        );
        assert!(report.contains("[frontmatter]"));
        assert!(report.contains("[secret-callout]"));
        Ok(())
    }

    #[test]
    fn reports_two_malformed_markdown_files_together() -> anyhow::Result<()> {
        let directory = tempdir()?;
        for name in ["first.md", "second.md"] {
            fs::write(
                directory.path().join(name),
                "---\ntype: location\n---\nIncomplete note",
            )?;
        }

        let error = scan_directory_with_stats(directory.path()).unwrap_err();
        let report = format!("{error:#}");

        assert!(report.contains(&format!(
            "2 corpus error(s) in {}:",
            directory.path().display()
        )));
        assert_eq!(
            report
                .matches(&directory.path().display().to_string())
                .count(),
            1
        );
        assert!(report.contains("first.md"));
        assert!(report.contains("second.md"));
        assert_eq!(report.matches("[frontmatter]").count(), 2);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_child_directory_is_reported_without_blocking_siblings() -> anyhow::Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempdir()?;
        let unreadable = directory.path().join("unreadable");
        fs::create_dir(&unreadable)?;
        fs::write(
            directory.path().join("sibling.md"),
            note("sibling", "canon"),
        )?;
        fs::write(unreadable.join("hidden.md"), note("hidden", "canon"))?;

        let original_mode = fs::metadata(&unreadable)?.permissions().mode();
        let mut permissions = fs::metadata(&unreadable)?.permissions();
        permissions.set_mode(0o0);
        fs::set_permissions(&unreadable, permissions)?;
        let scan =
            super::scan_directory_partial_with_stats_excluding(directory.path(), &HashSet::new())?;
        let mut restore = fs::metadata(&unreadable)?.permissions();
        restore.set_mode(original_mode);
        fs::set_permissions(&unreadable, restore)?;

        // Privileged test runners can still read mode-000 directories.
        if scan
            .errors
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.kind != CorpusErrorKind::DirectoryTraversal)
        {
            return Ok(());
        }
        assert!(
            scan.documents
                .iter()
                .any(|document| document.metadata.id == "sibling")
        );
        assert!(scan.errors.diagnostics.iter().any(|diagnostic| {
            diagnostic.kind == CorpusErrorKind::DirectoryTraversal
                && diagnostic.path.as_deref() == Some(unreadable.as_path())
        }));
        Ok(())
    }

    #[test]
    fn public_scan_returns_one_report_after_inspecting_all_siblings() -> anyhow::Result<()> {
        let directory = tempdir()?;
        let nested = directory.path().join("nested");
        fs::create_dir(&nested)?;
        fs::write(directory.path().join("valid.md"), note("valid", "canon"))?;
        fs::write(
            nested.join("bad-frontmatter.md"),
            "---\ntype: location\n---\nIncomplete note",
        )?;
        fs::write(
            directory.path().join("bad-secret.md"),
            format!(
                "{}\n\n> [!secret] GM notes\n> Hidden information.\n",
                note("bad-secret", "canon").replace("visibility: secret", "visibility: player")
            ),
        )?;

        let error = scan_directory_with_stats(directory.path()).unwrap_err();
        let report = format!("{error:#}");

        assert!(report.contains(&format!(
            "2 corpus error(s) in {}:",
            directory.path().display()
        )));
        assert_eq!(
            report
                .matches(&directory.path().display().to_string())
                .count(),
            1
        );
        assert!(report.contains("nested/bad-frontmatter.md"));
        assert!(report.contains("bad-secret.md"));
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
                .contains(&format!(
                    "index directory does not exist or is not a directory: {}",
                    file.display()
                ))
        );
        assert!(
            scan_directory_with_stats(directory.path().join("missing"))
                .unwrap_err()
                .to_string()
                .contains(&format!(
                    "index directory does not exist or is not a directory: {}",
                    directory.path().join("missing").display()
                ))
        );
        Ok(())
    }
}
