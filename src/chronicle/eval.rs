//! Retrieval evaluation only: no Discord connection, live database, or answer LLM.
use super::indexer::{
    db::{AccessScope, IndexerDb, SearchResult},
    embedder::Embedder,
    retriever::{RetrievalDiagnostics, SearchSettings, select_with_diagnostics},
    scanner,
    service::Indexer,
};
use anyhow::{Context, Result, ensure};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, OpenOptions, create_dir_all},
    path::Path,
    time::Instant,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Suite {
    name: String,
    max_chunk_tokens: usize,
    chunk_overlap_tokens: usize,
    minimum_hybrid_recall: f64,
    retrieval: SearchSettings,
    cases: Vec<Case>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    category: String,
    question: String,
    #[serde(default = "default_access_scope")]
    access: AccessScope,
    /// Stable frontmatter IDs. Empty means no answer in the fixture corpus.
    relevant_notes: Vec<String>,
    #[serde(default)]
    evidence: Vec<String>,
    #[serde(default)]
    forbidden_evidence: Vec<String>,
}

const fn default_access_scope() -> AccessScope {
    AccessScope::Player
}

#[derive(Debug, Serialize)]
struct Metrics {
    recall: Option<f64>,
    precision: Option<f64>,
    reciprocal_rank: Option<f64>,
    evidence_coverage: Option<f64>,
    returned_for_unanswerable: Option<bool>,
    forbidden_evidence_returned: bool,
}

#[derive(Serialize)]
struct ModeResult {
    notes: Vec<String>,
    metrics: Metrics,
    diagnostics: RetrievalDiagnostics,
}

#[derive(Serialize)]
struct CaseResult {
    case: Case,
    retrieval_ms: u128,
    modes: BTreeMap<String, ModeResult>,
}

#[derive(Serialize)]
struct Aggregate {
    answerable_cases: usize,
    mean_recall: f64,
    mean_precision: f64,
    mean_reciprocal_rank: f64,
    unanswerable_cases: usize,
    unanswerable_with_candidates: usize,
}

#[derive(Serialize)]
struct Report {
    suite: String,
    fixture_sha256: String,
    embedding_model: &'static str,
    embedding_revision: String,
    settings: SearchSettings,
    max_chunk_tokens: usize,
    chunk_overlap_tokens: usize,
    minimum_hybrid_recall: f64,
    visibility_passed: bool,
    passed: bool,
    aggregate: BTreeMap<String, Aggregate>,
    cases: Vec<CaseResult>,
}

#[allow(clippy::cast_precision_loss)]
fn ratio(n: usize, d: usize) -> f64 {
    if d == 0 { 0.0 } else { n as f64 / d as f64 }
}

fn metrics(case: &Case, notes: &[String], passages: &[String]) -> Metrics {
    let relevant = case.relevant_notes.iter().collect::<BTreeSet<_>>();
    let retrieved = notes.iter().collect::<BTreeSet<_>>();
    let answerable = !relevant.is_empty();
    let hits = relevant.intersection(&retrieved).count();
    Metrics {
        recall: answerable.then(|| ratio(hits, relevant.len())),
        precision: answerable.then(|| ratio(hits, retrieved.len())),
        reciprocal_rank: answerable.then(|| {
            notes
                .iter()
                .position(|n| relevant.contains(n))
                .map_or(0.0, |rank| ratio(1, rank + 1))
        }),
        evidence_coverage: (!case.evidence.is_empty()).then(|| {
            ratio(
                case.evidence
                    .iter()
                    .filter(|snippet| passages.iter().any(|p| p.contains(snippet.as_str())))
                    .count(),
                case.evidence.len(),
            )
        }),
        returned_for_unanswerable: (!answerable).then_some(!notes.is_empty()),
        forbidden_evidence_returned: case
            .forbidden_evidence
            .iter()
            .any(|snippet| passages.iter().any(|passage| passage.contains(snippet))),
    }
}

fn validate(
    suite: &Suite,
    identities: &BTreeMap<String, String>,
    contents: &BTreeMap<String, String>,
) -> Result<()> {
    suite.retrieval.validate()?;
    ensure!(!suite.cases.is_empty(), "Evaluation suite has no cases");
    ensure!(
        (0.0..=1.0).contains(&suite.minimum_hybrid_recall),
        "Invalid minimum recall"
    );
    ensure!(
        suite.max_chunk_tokens > 0
            && suite.max_chunk_tokens <= 512
            && suite.chunk_overlap_tokens < suite.max_chunk_tokens,
        "Invalid chunk settings"
    );
    ensure!(
        suite.cases.iter().any(|c| !c.relevant_notes.is_empty()),
        "Suite needs answerable cases"
    );
    let mut ids = BTreeSet::new();
    for case in &suite.cases {
        ensure!(
            !case.id.is_empty() && ids.insert(&case.id),
            "Empty or duplicate case ID: {}",
            case.id
        );
        ensure!(
            !case.question.trim().is_empty(),
            "Empty question: {}",
            case.id
        );
        let mut expected = BTreeSet::new();
        for id in &case.relevant_notes {
            ensure!(
                identities.values().any(|value| value == id),
                "Unknown relevant note {id} in {}",
                case.id
            );
            ensure!(
                expected.insert(id),
                "Duplicate relevant note in {}",
                case.id
            );
        }
        ensure!(
            !case.relevant_notes.is_empty() || case.evidence.is_empty(),
            "Unanswerable case has evidence"
        );
        for snippet in &case.evidence {
            ensure!(
                !snippet.is_empty()
                    && case
                        .relevant_notes
                        .iter()
                        .any(|id| contents.get(id).is_some_and(|c| c.contains(snippet))),
                "Missing evidence in {}: {snippet}",
                case.id
            );
        }
        for snippet in &case.forbidden_evidence {
            ensure!(
                !snippet.is_empty() && contents.values().any(|content| content.contains(snippet)),
                "Missing forbidden evidence in {}: {snippet}",
                case.id
            );
        }
    }
    Ok(())
}

fn mode_result(
    case: &Case,
    vector: Vec<SearchResult>,
    lexical: Vec<SearchResult>,
    settings: SearchSettings,
    identities: &BTreeMap<String, String>,
) -> Result<ModeResult> {
    let (results, mut diagnostics) = select_with_diagnostics(vector, lexical, settings);
    let notes = results
        .iter()
        .map(|r| {
            identities
                .get(&r.document_path)
                .cloned()
                .context("Retrieved note missing from fixture registry")
        })
        .collect::<Result<Vec<_>>>()?;
    let passages = results.iter().map(|r| r.text.clone()).collect::<Vec<_>>();
    for candidate in &mut diagnostics.candidates {
        candidate.document = identities
            .get(&candidate.document)
            .context("Diagnostic note missing from registry")?
            .clone();
    }
    Ok(ModeResult {
        metrics: metrics(case, &notes, &passages),
        notes,
        diagnostics,
    })
}

fn aggregate_results(cases: &[CaseResult]) -> BTreeMap<String, Aggregate> {
    let mut aggregate = BTreeMap::new();
    for name in ["lexical", "vector", "hybrid"] {
        let values = cases
            .iter()
            .map(|c| &c.modes[name].metrics)
            .collect::<Vec<_>>();
        let answerable_cases = values.iter().filter(|m| m.recall.is_some()).count();
        let mean = |sum: f64| sum * ratio(1, answerable_cases);
        aggregate.insert(
            name.into(),
            Aggregate {
                answerable_cases,
                mean_recall: mean(values.iter().filter_map(|m| m.recall).sum()),
                mean_precision: mean(values.iter().filter_map(|m| m.precision).sum()),
                mean_reciprocal_rank: mean(values.iter().filter_map(|m| m.reciprocal_rank).sum()),
                unanswerable_cases: values.len() - answerable_cases,
                unanswerable_with_candidates: values
                    .iter()
                    .filter(|m| m.returned_for_unanswerable == Some(true))
                    .count(),
            },
        );
    }
    aggregate
}

async fn evaluate_case(
    case: Case,
    database: &IndexerDb,
    embedder: &dyn super::indexer::embedder::EmbeddingModel,
    settings: SearchSettings,
    identities: &BTreeMap<String, String>,
) -> Result<CaseResult> {
    let start = Instant::now();
    let encoding = embedder.encode(&case.question)?;
    let embedding = embedder
        .embed_encodings(&[encoding])?
        .pop()
        .context("Missing query embedding")?;
    let (vector, lexical) = tokio::try_join!(
        database.search_similar_for(&embedding, settings.limits.candidate_limit, case.access),
        database.search_lexical_for(&case.question, settings.limits.candidate_limit, case.access)
    )?;
    let retrieval_ms = start.elapsed().as_millis();
    let mut modes = BTreeMap::new();
    modes.insert(
        "lexical".into(),
        mode_result(&case, Vec::new(), lexical.clone(), settings, identities)?,
    );
    modes.insert(
        "vector".into(),
        mode_result(&case, vector.clone(), Vec::new(), settings, identities)?,
    );
    modes.insert(
        "hybrid".into(),
        mode_result(&case, vector, lexical, settings, identities)?,
    );
    Ok(CaseResult {
        case,
        retrieval_ms,
        modes,
    })
}

fn create_report_file(
    requested_path: Option<&Path>,
    log_dir: &Path,
) -> Result<(File, std::path::PathBuf)> {
    if let Some(path) = requested_path {
        ensure!(!path.exists(), "Report path must be a new file");
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .context("Report path must be a new file")?;
        return Ok((file, path.to_owned()));
    }

    let directory = log_dir.join("evaluation");
    create_dir_all(&directory).with_context(|| {
        format!(
            "Failed to create report directory at {}",
            directory.display()
        )
    })?;
    let timestamp = Utc::now().format("%Y%m%d-%H%M%S");
    let mut suffix = 0_u64;
    loop {
        let name = if suffix == 0 {
            format!("chronicle-report-{timestamp}.json")
        } else {
            format!("chronicle-report-{timestamp}-{suffix}.json")
        };
        let path = directory.join(name);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((file, path)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => suffix += 1,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("Failed to create report file at {}", path.display())
                });
            }
        }
    }
}

fn fixture_registry(
    documents: &[super::indexer::document::Document],
) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
    let identities = documents
        .iter()
        .map(|document| {
            (
                document.path.to_string_lossy().into_owned(),
                document.metadata.id.clone(),
            )
        })
        .collect();
    let contents = documents
        .iter()
        .map(|document| {
            (
                document.metadata.id.clone(),
                format!(
                    "{}\n{}",
                    document.content,
                    document.secret_content.join("\n")
                ),
            )
        })
        .collect();
    (identities, contents)
}

fn fixture_fingerprint(
    source: &str,
    corpus: &Path,
    documents: &[super::indexer::document::Document],
) -> Result<String> {
    let mut hash = Sha256::new();
    hash.update(source.as_bytes());
    for document in documents {
        hash.update(
            document
                .path
                .strip_prefix(corpus)?
                .to_string_lossy()
                .as_bytes(),
        );
        hash.update(document.content_hash.as_bytes());
    }
    Ok(hex::encode(hash.finalize()))
}

pub async fn run(
    suite_path: &Path,
    requested_report_path: Option<&Path>,
    paths: &crate::chronicle::config::AppPaths,
) -> Result<()> {
    if let Some(path) = requested_report_path {
        ensure!(!path.exists(), "Report path must be a new file");
    }
    let source = std::fs::read_to_string(suite_path).context("Failed to read evaluation suite")?;
    let suite: Suite = toml::from_str(&source).context("Invalid evaluation suite")?;
    let corpus = suite_path
        .parent()
        .context("Suite needs a parent directory")?
        .join("corpus");
    let (documents, _) = scanner::scan_directory_with_stats(&corpus)?;
    let (identities, contents) = fixture_registry(&documents);
    validate(&suite, &identities, &contents)?;
    let fingerprint = fixture_fingerprint(&source, &corpus, &documents)?;
    let temporary = tempfile::tempdir()?;
    let database = IndexerDb::open(&format!(
        "sqlite://{}",
        temporary.path().join("chronicle.sqlite3").display()
    ))
    .await?;
    let embedder = Embedder::load(candle_core::Device::Cpu)?;
    let embedding_revision = embedder.revision().to_owned();
    let indexer = Indexer::new(
        corpus,
        database,
        embedder,
        suite.max_chunk_tokens,
        suite.chunk_overlap_tokens,
    );
    indexer.index().await?;
    let (database, embedder) = indexer.into_parts();
    let mut cases = Vec::new();
    for case in suite.cases {
        cases.push(
            evaluate_case(
                case,
                &database,
                embedder.as_ref(),
                suite.retrieval,
                &identities,
            )
            .await?,
        );
    }
    let aggregate = aggregate_results(&cases);
    let visibility_passed = cases.iter().all(|case| {
        case.modes
            .values()
            .all(|mode| !mode.metrics.forbidden_evidence_returned)
    });
    let passed =
        aggregate["hybrid"].mean_recall >= suite.minimum_hybrid_recall && visibility_passed;
    let report = Report {
        suite: suite.name,
        fixture_sha256: fingerprint,
        embedding_model: super::indexer::embedder::MODEL_ID,
        embedding_revision,
        settings: suite.retrieval,
        max_chunk_tokens: suite.max_chunk_tokens,
        chunk_overlap_tokens: suite.chunk_overlap_tokens,
        minimum_hybrid_recall: suite.minimum_hybrid_recall,
        visibility_passed,
        passed,
        aggregate,
        cases,
    };
    // Never overwrite a corpus file, suite, or previous baseline report.
    let (file, report_path) = create_report_file(requested_report_path, &paths.log_dir)?;
    serde_json::to_writer_pretty(file, &report)?;
    tracing::info!(report = %report_path.display(), recall = report.aggregate["hybrid"].mean_recall, passed, "Evaluation complete");
    ensure!(
        passed,
        "Retrieval evaluation failed; see {}",
        report_path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn fixture_lexical_retrieval_and_annotations_work_without_a_model() -> Result<()> {
        use crate::chronicle::indexer::db::IndexedChunk;
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/chronicle");
        let mut suite: Suite = toml::from_str(&std::fs::read_to_string(root.join("suite.toml"))?)?;
        let (documents, _) = scanner::scan_directory_with_stats(root.join("corpus"))?;
        let identities = documents
            .iter()
            .map(|d| (d.path.to_string_lossy().into_owned(), d.metadata.id.clone()))
            .collect::<BTreeMap<_, _>>();
        let contents = documents
            .iter()
            .map(|d| {
                (
                    d.metadata.id.clone(),
                    format!("{}\n{}", d.content, d.secret_content.join("\n")),
                )
            })
            .collect::<BTreeMap<_, _>>();
        validate(&suite, &identities, &contents)?;
        assert!(!identities.values().any(|id| id == "draft-crown"));
        let temp = tempfile::tempdir()?;
        let database = IndexerDb::open(&format!(
            "sqlite://{}",
            temp.path().join("fixture.sqlite3").display()
        ))
        .await?;
        // Deliberately whole-note chunks and dummy vectors: this checks fixture
        // ingestion and lexical mechanics, not semantic retrieval quality.
        for note in documents {
            let primary_visibility = if note.metadata.visibility == "secret" {
                crate::chronicle::indexer::document::ChunkVisibility::Secret
            } else {
                crate::chronicle::indexer::document::ChunkVisibility::Player
            };
            let mut chunks = vec![IndexedChunk {
                chunk_index: 0,
                heading: None,
                text: note.content,
                visibility: primary_visibility,
                overlaps_previous: false,
            }];
            chunks.extend(
                note.secret_content
                    .into_iter()
                    .enumerate()
                    .map(|(index, text)| IndexedChunk {
                        #[allow(clippy::cast_possible_wrap)]
                        chunk_index: (index + 1) as i64,
                        heading: Some("Secret".into()),
                        text,
                        visibility: crate::chronicle::indexer::document::ChunkVisibility::Secret,
                        overlaps_previous: false,
                    }),
            );
            let embeddings = vec![vec![0.0; 384]; chunks.len()];
            database
                .replace_note(
                    &note.path.to_string_lossy(),
                    &note.content_hash,
                    &chunks,
                    &embeddings,
                    &note.metadata,
                )
                .await?;
        }
        let result = database
            .search_lexical_for("Silver Beacon", 3, AccessScope::Gm)
            .await?;
        assert_eq!(identities[&result[0].document_path], "moonspire");
        assert!(
            database
                .search_lexical_for("VIOLETXYZZY", 3, AccessScope::Gm)
                .await?
                .is_empty()
        );
        let player_results = database
            .search_lexical_for("glass-comet password", 3, AccessScope::Player)
            .await?;
        assert!(
            player_results
                .iter()
                .all(|result| !result.text.contains("glass-comet password"))
        );
        assert_eq!(
            identities[&database
                .search_lexical_for("glass-comet password", 3, AccessScope::Gm)
                .await?[0]
                .document_path],
            "vault"
        );
        assert!(
            database
                .search_lexical_for("night-ink ledger", 3, AccessScope::Player)
                .await?
                .iter()
                .all(|result| !result.text.contains("night-ink ledger"))
        );
        suite.cases[0].relevant_notes.push("nonexistent".into());
        assert!(validate(&suite, &identities, &contents).is_err());
        Ok(())
    }

    #[test]
    fn scores_distinct_notes_rank_and_unknowns() {
        let mut case = Case {
            id: "test".into(),
            category: "test".into(),
            question: "q".into(),
            access: AccessScope::Player,
            relevant_notes: vec!["a".into(), "b".into()],
            evidence: vec!["fact".into()],
            forbidden_evidence: Vec::new(),
        };
        let score = metrics(
            &case,
            &["x".into(), "a".into(), "a".into()],
            &["a fact".into()],
        );
        assert_eq!(score.recall, Some(0.5));
        assert_eq!(score.precision, Some(0.5));
        assert_eq!(score.reciprocal_rank, Some(0.5));
        assert_eq!(score.evidence_coverage, Some(1.0));
        case.relevant_notes.clear();
        case.evidence.clear();
        let score = metrics(&case, &["x".into()], &[]);
        assert!(score.recall.is_none());
        assert_eq!(score.returned_for_unanswerable, Some(true));
    }
}
