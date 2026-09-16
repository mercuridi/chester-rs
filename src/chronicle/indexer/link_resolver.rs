//! Corpus-wide resolution of Obsidian-style wikilinks.
//!
//! This deliberately produces a derived manifest rather than rewriting note
//! text. The manifest is the seam used by the future persisted graph index.

use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
};

use anyhow::Result;

use super::scanner::DocumentCandidate;
use super::{document::Document, frontmatter::MetadataValue};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkVisibility {
    Player,
    Secret,
}

impl LinkVisibility {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Player => "player",
            Self::Secret => "secret",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkOrigin {
    Frontmatter { field_name: String },
    Body,
}

impl LinkOrigin {
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Frontmatter { .. } => "frontmatter",
            Self::Body => "body",
        }
    }

    pub fn field_name(&self) -> &str {
        match self {
            Self::Frontmatter { field_name } => field_name,
            Self::Body => "",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLink {
    pub source_note_id: String,
    pub target_note_id: String,
    pub origin: LinkOrigin,
    pub visibility: LinkVisibility,
    /// The authored wikilink, retained for diagnostics but not persistence.
    pub raw: String,
    /// An optional heading or block reference, not part of document identity.
    pub fragment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedLink {
    pub source_note_id: String,
    pub origin: LinkOrigin,
    pub visibility: LinkVisibility,
    pub raw: String,
}

#[derive(Debug, Default, Clone)]
pub struct LinkResolution {
    pub resolved: Vec<ResolvedLink>,
    pub dangling: Vec<UnresolvedLink>,
    pub ambiguous: Vec<UnresolvedLink>,
}

#[derive(Default)]
struct Catalogue {
    ids: HashMap<String, BTreeSet<String>>,
    paths: HashMap<String, BTreeSet<String>>,
    titles: HashMap<String, BTreeSet<String>>,
    aliases: HashMap<String, BTreeSet<String>>,
}

pub struct ResolverCatalogue(Catalogue);

#[allow(dead_code)] // Retained as the fail-fast API for non-indexer callers.
pub fn catalogue_from_candidates(
    root: &Path,
    candidates: &[DocumentCandidate],
) -> Result<ResolverCatalogue> {
    let (catalogue, errors) = catalogue_from_candidates_collecting(root, candidates);
    if let Some((_, error)) = errors.into_iter().next() {
        return Err(error);
    }
    Ok(catalogue)
}

pub(crate) fn catalogue_from_candidates_collecting(
    root: &Path,
    candidates: &[DocumentCandidate],
) -> (ResolverCatalogue, Vec<(PathBuf, anyhow::Error)>) {
    let mut catalogue = Catalogue::default();
    let mut errors = Vec::new();
    for candidate in candidates {
        let note_id = &candidate.metadata.id;
        Catalogue::insert(&mut catalogue.ids, identity_key(note_id), note_id);
        let relative = match candidate.path.strip_prefix(root) {
            Ok(relative) => relative,
            Err(error) => {
                errors.push((
                    candidate.path.clone(),
                    anyhow::Error::new(error).context(format!(
                        "Document path is outside index root: {}",
                        candidate.path.display()
                    )),
                ));
                continue;
            }
        };
        let relative = relative.to_string_lossy().replace('\\', "/");
        Catalogue::insert(&mut catalogue.paths, path_key(&relative), note_id);
        Catalogue::insert(
            &mut catalogue.titles,
            identity_key(
                &candidate
                    .path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy(),
            ),
            note_id,
        );
        for alias in &candidate.metadata.aliases {
            Catalogue::insert(&mut catalogue.aliases, identity_key(alias), note_id);
        }
    }
    errors.sort_by(|left, right| left.0.cmp(&right.0));
    (ResolverCatalogue(catalogue), errors)
}

pub fn resolve_document(catalogue: &ResolverCatalogue, document: &Document) -> LinkResolution {
    let mut outcome = LinkResolution::default();
    let source_note_id = document.metadata.id.clone();
    let visibility = document_visibility(document);
    for (field_name, value) in &document.metadata.fields {
        for raw in frontmatter_links(value) {
            record(
                &mut outcome,
                &catalogue.0,
                &source_note_id,
                LinkOrigin::Frontmatter {
                    field_name: field_name.clone(),
                },
                visibility,
                raw,
            );
        }
    }
    for raw in extract_wikilinks(&document.public_body) {
        record(
            &mut outcome,
            &catalogue.0,
            &source_note_id,
            LinkOrigin::Body,
            visibility,
            raw,
        );
    }
    for body in &document.secret_bodies {
        for raw in extract_wikilinks(body) {
            record(
                &mut outcome,
                &catalogue.0,
                &source_note_id,
                LinkOrigin::Body,
                LinkVisibility::Secret,
                raw,
            );
        }
    }
    outcome
}

impl Catalogue {
    fn insert(map: &mut HashMap<String, BTreeSet<String>>, key: String, note_id: &str) {
        if !key.is_empty() {
            map.entry(key).or_default().insert(note_id.to_owned());
        }
    }

    fn resolve(&self, target: &str) -> Resolution {
        for candidates in [
            self.ids.get(&identity_key(target)),
            self.paths.get(&path_key(target)),
            self.titles.get(&identity_key(target)),
            self.aliases.get(&identity_key(target)),
        ]
        .into_iter()
        .flatten()
        {
            if candidates.len() != 1 {
                return Resolution::Ambiguous;
            }
            if let Some(candidate) = candidates.iter().next() {
                return Resolution::Resolved(candidate.to_owned());
            }
        }
        Resolution::Dangling
    }
}

enum Resolution {
    Resolved(String),
    Dangling,
    Ambiguous,
}

fn record(
    outcome: &mut LinkResolution,
    catalogue: &Catalogue,
    source_note_id: &str,
    origin: LinkOrigin,
    visibility: LinkVisibility,
    raw: RawWikilink,
) {
    let unresolved = || UnresolvedLink {
        source_note_id: source_note_id.to_owned(),
        origin: origin.clone(),
        visibility,
        raw: raw.raw.clone(),
    };
    match catalogue.resolve(&raw.target) {
        Resolution::Resolved(target_note_id) => outcome.resolved.push(ResolvedLink {
            source_note_id: source_note_id.to_owned(),
            target_note_id,
            origin,
            visibility,
            raw: raw.raw,
            fragment: raw.fragment,
        }),
        Resolution::Dangling => outcome.dangling.push(unresolved()),
        Resolution::Ambiguous => outcome.ambiguous.push(unresolved()),
    }
}

fn document_visibility(document: &Document) -> LinkVisibility {
    if document.metadata.visibility == "secret" {
        LinkVisibility::Secret
    } else {
        LinkVisibility::Player
    }
}

fn frontmatter_links(value: &MetadataValue) -> Vec<RawWikilink> {
    match value {
        MetadataValue::Wikilink(value) => parse_wikilink(value).into_iter().collect(),
        MetadataValue::WikilinkList(values) => values
            .iter()
            .filter_map(|value| parse_wikilink(value))
            .collect(),
        MetadataValue::StringOrWikilink(value) if value.trim_start().starts_with("[[") => {
            parse_wikilink(value).into_iter().collect()
        }
        _ => Vec::new(),
    }
}

#[derive(Debug, Clone)]
struct RawWikilink {
    raw: String,
    target: String,
    fragment: Option<String>,
}

/// Extract valid, non-empty wikilinks while ignoring fenced code examples.
fn extract_wikilinks(source: &str) -> Vec<RawWikilink> {
    let mut links = Vec::new();
    let mut in_fence = false;
    for line in source.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let mut remainder = line;
        while let Some(start) = remainder.find("[[") {
            let after_start = &remainder[start + 2..];
            let Some(end) = after_start.find("]]") else {
                break;
            };
            let raw = &remainder[start..start + 2 + end + 2];
            if let Some(link) = parse_wikilink(raw) {
                links.push(link);
            }
            remainder = &after_start[end + 2..];
        }
    }
    links
}

/// Normalize aliases, headings, and block references to a document target.
fn parse_wikilink(raw: &str) -> Option<RawWikilink> {
    let raw = raw.trim();
    let inner = raw.strip_prefix("[[")?.strip_suffix("]]")?.trim();
    let destination = inner
        .split_once('|')
        .map_or(inner, |(target, _)| target)
        .trim();
    let fragment_start = destination
        .char_indices()
        .find_map(|(index, character)| matches!(character, '#' | '^').then_some(index));
    let (target, fragment) = fragment_start.map_or((destination, None), |index| {
        let (target, fragment) = destination.split_at(index);
        let fragment = fragment[1..].trim();
        (
            target,
            (!fragment.is_empty()).then_some(fragment.to_owned()),
        )
    });
    let target = target.trim();
    (!target.is_empty()).then(|| RawWikilink {
        raw: raw.to_owned(),
        target: target.to_owned(),
        fragment,
    })
}

fn identity_key(value: &str) -> String {
    value.trim().to_lowercase()
}

fn path_key(value: &str) -> String {
    let value = value.trim().trim_matches('/');
    let value = value
        .strip_suffix(".md")
        .or_else(|| value.strip_suffix(".MD"))
        .unwrap_or(value);
    value
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .collect::<Vec<_>>()
        .join("/")
        .to_lowercase()
}

#[cfg(test)]
#[allow(clippy::too_many_arguments, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::chronicle::indexer::frontmatter;

    fn document(
        root: &Path,
        relative_path: &str,
        id: &str,
        aliases: &[&str],
        visibility: &str,
        public_body: &str,
        secret_bodies: Vec<&str>,
        extra: &str,
    ) -> Document {
        let aliases = aliases.join(", ");
        let source = format!(
            "---\nid: {id}\ntype: character\nstatus: canon\nvisibility: {visibility}\ncreated: 2026-09-07\nupdated: 2026-09-07\naliases: [{aliases}]\n{extra}---\n"
        );
        let (metadata, _) = frontmatter::parse(&source).unwrap().unwrap();
        Document {
            metadata,
            path: root.join(relative_path),
            content: public_body.to_owned(),
            public_body: public_body.to_owned(),
            secret_content: Vec::new(),
            secret_bodies: secret_bodies.into_iter().map(str::to_owned).collect(),
            content_hash: String::new(),
        }
    }

    fn resolve_documents(root: &Path, documents: &[Document]) -> Result<LinkResolution> {
        let candidates = documents
            .iter()
            .map(|document| DocumentCandidate {
                path: document.path.clone(),
                metadata: document.metadata.clone(),
                content_hash: document.content_hash.clone(),
            })
            .collect::<Vec<_>>();
        let catalogue = catalogue_from_candidates(root, &candidates)?;
        let mut outcome = LinkResolution::default();
        for document in documents {
            let resolved = resolve_document(&catalogue, document);
            outcome.resolved.extend(resolved.resolved);
            outcome.dangling.extend(resolved.dangling);
            outcome.ambiguous.extend(resolved.ambiguous);
        }
        Ok(outcome)
    }

    #[test]
    fn collecting_catalogue_keeps_valid_candidates_after_path_errors() {
        let root = Path::new("/vault");
        let documents = [
            document(root, "Valid.md", "valid", &[], "player", "", vec![], ""),
            document(
                Path::new("/outside"),
                "First.md",
                "first",
                &[],
                "player",
                "",
                vec![],
                "",
            ),
            document(
                Path::new("/elsewhere"),
                "Second.md",
                "second",
                &[],
                "player",
                "",
                vec![],
                "",
            ),
        ];
        let candidates = documents
            .iter()
            .map(|document| DocumentCandidate {
                path: document.path.clone(),
                metadata: document.metadata.clone(),
                content_hash: document.content_hash.clone(),
            })
            .collect::<Vec<_>>();

        let (catalogue, errors) = catalogue_from_candidates_collecting(root, &candidates);

        assert_eq!(errors.len(), 2);
        assert_eq!(errors[0].0, Path::new("/elsewhere/Second.md"));
        assert_eq!(errors[1].0, Path::new("/outside/First.md"));
        let resolved = resolve_document(&catalogue, &documents[0]);
        assert!(resolved.resolved.is_empty());
    }

    #[test]
    fn normalizes_alias_heading_block_and_vault_path_links() -> Result<()> {
        let root = Path::new("/vault");
        let documents = vec![
            document(
                root,
                "Places/Moonspire.md",
                "moon-id",
                &["Tower"],
                "player",
                "",
                vec![],
                "",
            ),
            document(
                root,
                "Source.md",
                "source",
                &[],
                "player",
                "[[moon-id|the tower]] [[Tower#History]] [[Places/Moonspire^founding-note]]",
                vec![],
                "",
            ),
        ];
        let outcome = resolve_documents(root, &documents)?;
        assert_eq!(outcome.resolved.len(), 3);
        assert!(
            outcome
                .resolved
                .iter()
                .all(|link| link.target_note_id == "moon-id")
        );
        assert_eq!(outcome.resolved[1].fragment.as_deref(), Some("History"));
        assert_eq!(
            outcome.resolved[2].fragment.as_deref(),
            Some("founding-note")
        );
        Ok(())
    }

    #[test]
    fn ids_take_precedence_and_ambiguous_aliases_do_not_resolve() -> Result<()> {
        let root = Path::new("/vault");
        let documents = vec![
            document(root, "Elsewhere.md", "moon", &[], "player", "", vec![], ""),
            document(
                root,
                "Moon.md",
                "other",
                &["shared"],
                "player",
                "",
                vec![],
                "",
            ),
            document(
                root,
                "Another.md",
                "another",
                &["shared"],
                "player",
                "",
                vec![],
                "",
            ),
            document(
                root,
                "Source.md",
                "source",
                &[],
                "player",
                "[[Moon]] [[shared]]",
                vec![],
                "",
            ),
        ];
        let outcome = resolve_documents(root, &documents)?;
        assert_eq!(outcome.resolved.len(), 1);
        assert_eq!(outcome.resolved[0].target_note_id, "moon");
        assert_eq!(outcome.ambiguous.len(), 1);
        Ok(())
    }

    #[test]
    fn retains_frontmatter_provenance_and_secret_visibility() -> Result<()> {
        let root = Path::new("/vault");
        let documents = vec![
            document(root, "Target.md", "target", &[], "player", "", vec![], ""),
            document(
                root,
                "Source.md",
                "source",
                &[],
                "mixed",
                "A public [[Target]].",
                vec!["A secret [[Target#Hidden]]."],
                "location: '[[Target]]'\n",
            ),
        ];
        let outcome = resolve_documents(root, &documents)?;
        assert_eq!(outcome.resolved.len(), 3);
        assert!(outcome.resolved.iter().any(|link| matches!(
            &link.origin,
            LinkOrigin::Frontmatter { field_name } if field_name == "location"
        ) && link.visibility == LinkVisibility::Player));
        assert!(outcome.resolved.iter().any(|link| {
            link.origin == LinkOrigin::Body
                && link.visibility == LinkVisibility::Secret
                && link.fragment.as_deref() == Some("Hidden")
        }));
        Ok(())
    }

    #[test]
    fn ignores_fenced_examples_and_reports_dangling_links() -> Result<()> {
        let root = Path::new("/vault");
        let documents = vec![document(
            root,
            "Source.md",
            "source",
            &[],
            "player",
            "```md\n[[Example]]\n```\n[[Missing]]",
            vec![],
            "",
        )];
        let outcome = resolve_documents(root, &documents)?;
        assert!(outcome.resolved.is_empty());
        assert_eq!(outcome.dangling.len(), 1);
        assert_eq!(outcome.dangling[0].raw, "[[Missing]]");
        Ok(())
    }
}
