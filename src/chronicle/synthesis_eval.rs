//! Model-backed bounded-synthesis evaluation with deterministic rubric scoring.
use super::{
    config::Config,
    indexer::{db::repository::IndexerDb, embedder::Embedder, service::Indexer},
    llm::Llm,
    query::{plan::Plan, planner},
    runtime::GpuRuntime,
    service::Chronicle,
};
use anyhow::{Context, Result, ensure};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::{
    fs::OpenOptions,
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Suite {
    name: String,
    minimum_required_fact_recall: f64,
    maximum_prohibited_claims: usize,
    minimum_gap_recall: f64,
    topology: Topology,
    safety: Safety,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Topology {
    document_count: usize,
    minimum_multi_document_cases: usize,
    required_categories: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Safety {
    gm_only_note: String,
    draft_contradiction_note: String,
    instruction_like_note: String,
    inaccessible_note: String,
    mixed_event_note: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FactExpectation {
    id: String,
    claim: String,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    entities: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProhibitedExpectation {
    id: String,
    claim: String,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    entities: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct GapExpectation {
    id: String,
    claim: String,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    entities: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawExpectation {
    Text(String),
    Structured {
        id: String,
        claim: String,
        #[serde(default)]
        aliases: Vec<String>,
        #[serde(default)]
        entities: Vec<String>,
    },
}

fn expectation_id(prefix: &str, index: usize) -> String {
    format!("{prefix}-{}", index + 1)
}

fn deserialize_fact_expectations<'de, D>(deserializer: D) -> Result<Vec<FactExpectation>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Vec::<RawExpectation>::deserialize(deserializer)?
        .into_iter()
        .enumerate()
        .map(|(index, expectation)| match expectation {
            RawExpectation::Text(claim) => Ok(FactExpectation {
                id: expectation_id("required-fact", index),
                claim,
                aliases: Vec::new(),
                entities: Vec::new(),
            }),
            RawExpectation::Structured {
                id,
                claim,
                aliases,
                entities,
            } => Ok(FactExpectation {
                id,
                claim,
                aliases,
                entities,
            }),
        })
        .collect()
}

fn deserialize_prohibited_expectations<'de, D>(
    deserializer: D,
) -> Result<Vec<ProhibitedExpectation>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Vec::<RawExpectation>::deserialize(deserializer)?
        .into_iter()
        .enumerate()
        .map(|(index, expectation)| match expectation {
            RawExpectation::Text(claim) => Ok(ProhibitedExpectation {
                id: expectation_id("prohibited-claim", index),
                claim,
                aliases: Vec::new(),
                entities: Vec::new(),
            }),
            RawExpectation::Structured {
                id,
                claim,
                aliases,
                entities,
            } => Ok(ProhibitedExpectation {
                id,
                claim,
                aliases,
                entities,
            }),
        })
        .collect()
}

fn deserialize_gap_expectations<'de, D>(deserializer: D) -> Result<Vec<GapExpectation>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Vec::<RawExpectation>::deserialize(deserializer)?
        .into_iter()
        .enumerate()
        .map(|(index, expectation)| match expectation {
            RawExpectation::Text(claim) => Ok(GapExpectation {
                id: expectation_id("expected-gap", index),
                claim,
                aliases: Vec::new(),
                entities: Vec::new(),
            }),
            RawExpectation::Structured {
                id,
                claim,
                aliases,
                entities,
            } => Ok(GapExpectation {
                id,
                claim,
                aliases,
                entities,
            }),
        })
        .collect()
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    question: String,
    expected_route: String,
    #[serde(deserialize_with = "deserialize_fact_expectations")]
    required_facts: Vec<FactExpectation>,
    #[serde(deserialize_with = "deserialize_prohibited_expectations")]
    prohibited_claims: Vec<ProhibitedExpectation>,
    #[serde(deserialize_with = "deserialize_gap_expectations")]
    expected_gaps: Vec<GapExpectation>,
}

#[derive(Debug, Serialize)]
struct ClaimResult {
    id: String,
    claim: String,
    matched: bool,
    status: ClaimStatus,
    matched_by: Option<String>,
    missing_entities: Vec<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ClaimStatus {
    Resolved,
    Unresolved,
    Contradicted,
}

#[derive(Debug, Serialize)]
struct CaseReport {
    case: Case,
    answer: String,
    synthesis_diagnostics: super::service::SynthesisDiagnostics,
    route_correct: bool,
    required_fact_recall: f64,
    prohibited_claims_found: Vec<String>,
    gap_recall: f64,
    required_fact_results: Vec<ClaimResult>,
    prohibited_claim_results: Vec<ClaimResult>,
    gap_results: Vec<ClaimResult>,
    passed: bool,
}

#[derive(Serialize)]
struct Report {
    suite: String,
    model: String,
    minimum_required_fact_recall: f64,
    maximum_prohibited_claims: usize,
    minimum_gap_recall: f64,
    passed: bool,
    cases: Vec<CaseReport>,
}

fn normalise(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character.is_alphanumeric() || character == '\'' {
                character.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

trait Expectation {
    fn id(&self) -> &str;
    fn claim(&self) -> &str;
    fn aliases(&self) -> &[String];
    fn entities(&self) -> &[String];
}

impl Expectation for FactExpectation {
    fn id(&self) -> &str {
        &self.id
    }

    fn claim(&self) -> &str {
        &self.claim
    }

    fn aliases(&self) -> &[String] {
        &self.aliases
    }

    fn entities(&self) -> &[String] {
        &self.entities
    }
}

impl Expectation for ProhibitedExpectation {
    fn id(&self) -> &str {
        &self.id
    }

    fn claim(&self) -> &str {
        &self.claim
    }

    fn aliases(&self) -> &[String] {
        &self.aliases
    }

    fn entities(&self) -> &[String] {
        &self.entities
    }
}

impl Expectation for GapExpectation {
    fn id(&self) -> &str {
        &self.id
    }

    fn claim(&self) -> &str {
        &self.claim
    }

    fn aliases(&self) -> &[String] {
        &self.aliases
    }

    fn entities(&self) -> &[String] {
        &self.entities
    }
}

const NEGATION_MARKERS: &[&str] = &[
    "cannot", "can't", "did not", "didn't", "does not", "doesn't", "do not", "don't", "is not",
    "isn't", "are not", "aren't", "was not", "wasn't", "were not", "weren't", "will not", "won't",
    "never", "no", "not",
];

fn without_negation(text: &str) -> String {
    let tokens = normalise(text)
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let markers = NEGATION_MARKERS
        .iter()
        .map(|marker| {
            normalise(marker)
                .split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let mut result = Vec::with_capacity(tokens.len());
    let mut index = 0;
    while index < tokens.len() {
        let matched_marker = markers.iter().find(|marker| {
            tokens
                .get(index..index + marker.len())
                .is_some_and(|window| window == marker.as_slice())
        });
        if let Some(marker) = matched_marker {
            index += marker.len();
        } else {
            result.push(tokens[index].clone());
            index += 1;
        }
    }
    result.join(" ")
}

#[derive(Debug, PartialEq, Eq)]
struct Match {
    positive: bool,
    negated: bool,
    matched_by: Option<String>,
    missing_entities: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
enum ExpectationKind {
    Required,
    Prohibited,
    Gap,
}

fn find_match<T: Expectation>(answer: &str, expectation: &T) -> Match {
    let answer = normalise(answer);
    let answer_without_negation = without_negation(answer.as_str());
    let missing_entities = expectation
        .entities()
        .iter()
        .filter(|entity| !answer.contains(&normalise(entity)))
        .cloned()
        .collect::<Vec<_>>();

    let candidates = std::iter::once(expectation.claim())
        .chain(expectation.aliases().iter().map(String::as_str));
    let mut matched_by = None;
    let mut positive = false;
    let mut negated = false;
    for candidate in candidates {
        let candidate = normalise(candidate);
        if answer.contains(&candidate) {
            positive = true;
            matched_by = Some(candidate);
            break;
        }
        if answer_without_negation.contains(&candidate) {
            negated = true;
            matched_by = Some(candidate);
            break;
        }
    }
    Match {
        positive,
        negated,
        matched_by,
        missing_entities,
    }
}

fn claim_results<T: Expectation>(
    answer: &str,
    expectations: &[T],
    kind: ExpectationKind,
) -> Vec<ClaimResult> {
    expectations
        .iter()
        .map(|expectation| {
            let found = find_match(answer, expectation);
            let entity_complete = found.missing_entities.is_empty();
            let matched = found.positive && entity_complete;
            let status = match kind {
                ExpectationKind::Required | ExpectationKind::Gap => {
                    if matched {
                        ClaimStatus::Resolved
                    } else if found.negated && entity_complete {
                        ClaimStatus::Contradicted
                    } else {
                        ClaimStatus::Unresolved
                    }
                }
                ExpectationKind::Prohibited => {
                    if matched {
                        ClaimStatus::Contradicted
                    } else {
                        ClaimStatus::Resolved
                    }
                }
            };
            ClaimResult {
                id: expectation.id().to_owned(),
                claim: expectation.claim().to_owned(),
                matched,
                status,
                matched_by: found.matched_by,
                missing_entities: found.missing_entities,
            }
        })
        .collect()
}

fn coverage<T: Expectation>(answer: &str, expectations: &[T], kind: ExpectationKind) -> f64 {
    if expectations.is_empty() {
        return 1.0;
    }
    #[allow(clippy::cast_precision_loss)]
    {
        claim_results(answer, expectations, kind)
            .iter()
            .filter(|result| result.status == ClaimStatus::Resolved)
            .count() as f64
            / expectations.len() as f64
    }
}

fn prohibited(answer: &str, expectations: &[ProhibitedExpectation]) -> Vec<String> {
    claim_results(answer, expectations, ExpectationKind::Prohibited)
        .into_iter()
        .filter(|result| result.status == ClaimStatus::Contradicted)
        .map(|result| result.claim)
        .collect()
}

fn validate_expectation_ids<T: Expectation>(
    expectations: &[T],
    case_id: &str,
    kind: &str,
) -> Result<()> {
    let mut ids = std::collections::HashSet::new();
    for expectation in expectations {
        ensure!(
            !expectation.id().trim().is_empty() && ids.insert(expectation.id()),
            "Duplicate or empty {kind} ID in case: {case_id}"
        );
        ensure!(
            !expectation.claim().trim().is_empty(),
            "Empty {kind} claim in case: {case_id}"
        );
    }
    Ok(())
}

fn validate_fixture_metadata(topology: &Topology, safety: &Safety) -> Result<()> {
    ensure!(
        topology.document_count > 0,
        "Synthesis topology has no documents"
    );
    ensure!(
        topology.minimum_multi_document_cases <= topology.document_count,
        "Synthesis topology has an invalid multi-document case minimum"
    );
    ensure!(
        !topology.required_categories.is_empty()
            && topology
                .required_categories
                .iter()
                .all(|category| !category.trim().is_empty()),
        "Synthesis topology has no valid required categories"
    );
    ensure!(
        [
            &safety.gm_only_note,
            &safety.draft_contradiction_note,
            &safety.instruction_like_note,
            &safety.inaccessible_note,
            &safety.mixed_event_note,
        ]
        .into_iter()
        .all(|note| !note.trim().is_empty()),
        "Synthesis safety metadata contains an empty note ID"
    );
    Ok(())
}

fn validate(suite: &Suite) -> Result<()> {
    ensure!(!suite.cases.is_empty(), "Synthesis suite has no cases");
    ensure!((0.0..=1.0).contains(&suite.minimum_required_fact_recall));
    ensure!((0.0..=1.0).contains(&suite.minimum_gap_recall));
    validate_fixture_metadata(&suite.topology, &suite.safety)?;
    let mut ids = std::collections::HashSet::new();
    for case in &suite.cases {
        ensure!(
            !case.id.is_empty() && ids.insert(&case.id),
            "Duplicate or empty case ID"
        );
        ensure!(!case.question.trim().is_empty(), "Empty synthesis question");
        ensure!(
            case.expected_route == "synthesis",
            "Synthesis cases must expect synthesis route"
        );
        ensure!(
            !case.required_facts.is_empty(),
            "Case has no required facts: {}",
            case.id
        );
        ensure!(
            !case.prohibited_claims.is_empty(),
            "Case has no prohibited claims: {}",
            case.id
        );
        validate_expectation_ids(&case.required_facts, &case.id, "required fact")?;
        validate_expectation_ids(&case.prohibited_claims, &case.id, "prohibited claim")?;
        validate_expectation_ids(&case.expected_gaps, &case.id, "expected gap")?;
    }
    Ok(())
}

fn create_report_file(requested: Option<&Path>) -> Result<(std::fs::File, PathBuf)> {
    let path = requested.map(Path::to_path_buf).unwrap_or_else(|| {
        PathBuf::from(format!(
            "chronicle-synthesis-report-{}.json",
            Utc::now().format("%Y%m%d-%H%M%S")
        ))
    });
    ensure!(!path.exists(), "Report path must be a new file");
    Ok((
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?,
        path,
    ))
}

pub async fn run(suite_path: &Path, requested_report: Option<&Path>) -> Result<()> {
    let suite: Suite = toml::from_str(&std::fs::read_to_string(suite_path)?)?;
    validate(&suite)?;
    let config_path = Path::new(env!("CARGO_MANIFEST_DIR")).join(".chronicle/config.toml");
    let config = Config::load(&config_path)?;
    let corpus = suite_path
        .parent()
        .context("Suite needs a parent directory")?
        .join("corpus");
    let temporary = tempfile::tempdir()?;
    let database = IndexerDb::open(&format!(
        "sqlite://{}",
        temporary.path().join("chronicle.sqlite3").display()
    ))
    .await?;
    let embedder = Embedder::load(candle_core::Device::Cpu)?;
    let indexer = Indexer::new(
        corpus,
        database,
        embedder,
        config.chronicle.max_chunk_tokens,
        config.chronicle.chunk_overlap_tokens,
    );
    indexer.index().await?;
    let (database, _) = indexer.into_parts();
    let runtime = GpuRuntime::new();
    let llm = Llm::new(&config.chronicle, runtime.clone());
    let model = format!(
        "{}@{} / {}",
        config.chronicle.llm_repo, config.chronicle.llm_revision, config.chronicle.llm_model_file
    );
    let chronicle = Chronicle::new(
        database,
        llm.clone(),
        runtime.clone(),
        config.chronicle.retrieval_limit,
        config.chronicle.retrieval_candidate_limit,
        config.chronicle.retrieval_distance_threshold,
        config.chronicle.retrieval_near_duplicate_threshold,
        config.chronicle.retrieval_max_chunks_per_document,
        config.chronicle.synthesis,
        config.chronicle.llm_max_reply_length,
    );
    chronicle.start_llm().await?;
    let mut cases = Vec::new();
    for case in suite.cases {
        let route =
            planner::parse_for_question(&case.question, &llm.generate_plan(&case.question).await?)?;
        let answer = chronicle.ask(&case.question).await?;
        let synthesis_diagnostics = chronicle
            .last_synthesis_diagnostics()?
            .context("Synthesis did not produce diagnostics")?;
        let route_correct = route == Plan::Synthesis {};
        let required_fact_results =
            claim_results(&answer, &case.required_facts, ExpectationKind::Required);
        let prohibited_claim_results = claim_results(
            &answer,
            &case.prohibited_claims,
            ExpectationKind::Prohibited,
        );
        let gap_results = claim_results(&answer, &case.expected_gaps, ExpectationKind::Gap);
        let required_fact_recall =
            coverage(&answer, &case.required_facts, ExpectationKind::Required);
        let prohibited_claims_found = prohibited(&answer, &case.prohibited_claims);
        let gap_recall = coverage(&answer, &case.expected_gaps, ExpectationKind::Gap);
        let passed = route_correct
            && required_fact_recall >= suite.minimum_required_fact_recall
            && prohibited_claims_found.len() <= suite.maximum_prohibited_claims
            && gap_recall >= suite.minimum_gap_recall;
        cases.push(CaseReport {
            case,
            answer,
            synthesis_diagnostics,
            route_correct,
            required_fact_recall,
            prohibited_claims_found,
            gap_recall,
            required_fact_results,
            prohibited_claim_results,
            gap_results,
            passed,
        });
    }
    chronicle.stop_llm().await?;
    let passed = cases.iter().all(|case| case.passed);
    let report = Report {
        suite: suite.name,
        model,
        minimum_required_fact_recall: suite.minimum_required_fact_recall,
        maximum_prohibited_claims: suite.maximum_prohibited_claims,
        minimum_gap_recall: suite.minimum_gap_recall,
        passed,
        cases,
    };
    let (file, path) = create_report_file(requested_report)?;
    serde_json::to_writer_pretty(file, &report)?;
    ensure!(
        passed,
        "Synthesis evaluation failed; see {}",
        path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scores_paraphrase_insensitively_and_flags_prohibited_claims() {
        let answer =
            "The kingdom was founded when the river settlements ratified the Ember Compact.";
        assert_eq!(
            coverage(
                answer,
                &[FactExpectation {
                    id: "foundation".into(),
                    claim: "  founded when the river settlements ratified the Ember Compact "
                        .into(),
                    aliases: Vec::new(),
                    entities: Vec::new(),
                }],
                ExpectationKind::Required,
            ),
            1.0
        );
        assert_eq!(
            coverage(
                answer,
                &[FactExpectation {
                    id: "foundation".into(),
                    claim: "The kingdom was founded.".into(),
                    aliases: vec!["The realm was established.".into()],
                    entities: vec!["kingdom".into()],
                }],
                ExpectationKind::Required,
            ),
            1.0
        );
        assert_eq!(
            prohibited(
                answer,
                &[ProhibitedExpectation {
                    id: "still-exists".into(),
                    claim: "the kingdom still exists".into(),
                    aliases: Vec::new(),
                    entities: Vec::new(),
                }],
            ),
            Vec::<String>::new()
        );
        assert_eq!(
            prohibited(
                "The kingdom still exists.",
                &[ProhibitedExpectation {
                    id: "still-exists".into(),
                    claim: "the kingdom still exists".into(),
                    aliases: Vec::new(),
                    entities: Vec::new(),
                }],
            )
            .len(),
            1
        );
    }

    #[test]
    fn parses_legacy_string_rubrics_and_assigns_stable_ids() -> Result<()> {
        let suite: Suite = toml::from_str(
            r#"
name = "legacy"
minimum_required_fact_recall = 0.5
maximum_prohibited_claims = 0
minimum_gap_recall = 0.0

[topology]
document_count = 1
minimum_multi_document_cases = 1
required_categories = ["chronology"]

[safety]
gm_only_note = "gm"
draft_contradiction_note = "draft"
instruction_like_note = "instruction"
inaccessible_note = "inaccessible"
mixed_event_note = "mixed"

[[cases]]
id = "case"
question = "What happened?"
expected_route = "synthesis"
required_facts = ["A happened."]
prohibited_claims = ["B happened."]
expected_gaps = ["The interval is unknown."]
"#,
        )?;

        assert_eq!(suite.cases[0].required_facts[0].id, "required-fact-1");
        assert_eq!(suite.cases[0].prohibited_claims[0].id, "prohibited-claim-1");
        assert_eq!(suite.cases[0].expected_gaps[0].id, "expected-gap-1");
        Ok(())
    }

    #[test]
    fn parses_structured_rubrics() -> Result<()> {
        let suite: Suite = toml::from_str(
            r#"
name = "structured"
minimum_required_fact_recall = 0.5
maximum_prohibited_claims = 0
minimum_gap_recall = 0.0

[topology]
document_count = 1
minimum_multi_document_cases = 1
required_categories = ["chronology"]

[safety]
gm_only_note = "gm"
draft_contradiction_note = "draft"
instruction_like_note = "instruction"
inaccessible_note = "inaccessible"
mixed_event_note = "mixed"

[[cases]]
id = "case"
question = "What happened?"
expected_route = "synthesis"

[[cases.required_facts]]
id = "foundation"
claim = "The kingdom was founded."
aliases = ["The realm was established."]
entities = ["kingdom"]

[[cases.prohibited_claims]]
id = "present-day"
claim = "The kingdom still exists."

[[cases.expected_gaps]]
id = "missing-interval"
claim = "The interval is unknown."
"#,
        )?;

        assert_eq!(suite.cases[0].required_facts[0].id, "foundation");
        assert_eq!(
            suite.cases[0].required_facts[0].claim,
            "The kingdom was founded."
        );
        assert_eq!(
            suite.cases[0].required_facts[0].aliases,
            vec!["The realm was established."]
        );
        assert_eq!(suite.cases[0].required_facts[0].entities, vec!["kingdom"]);
        Ok(())
    }

    #[test]
    fn aliases_resolve_a_claim_and_report_the_matching_alias() {
        let expectation = FactExpectation {
            id: "crown-relocation".into(),
            claim: "The crown moved from Ashford to Lantern Bay.".into(),
            aliases: vec!["The court relocated to Lantern Bay.".into()],
            entities: vec!["Lantern Bay".into()],
        };
        let results = claim_results(
            "After the flood, the court relocated to Lantern Bay.",
            &[expectation],
            ExpectationKind::Required,
        );

        assert_eq!(results[0].status, ClaimStatus::Resolved);
        assert_eq!(
            results[0].matched_by.as_deref(),
            Some("the court relocated to lantern bay")
        );
        assert!(results[0].missing_entities.is_empty());
    }

    #[test]
    fn entity_checks_leave_an_otherwise_matching_alias_unresolved() {
        let expectation = FactExpectation {
            id: "crown-relocation".into(),
            claim: "The crown moved to Lantern Bay.".into(),
            aliases: vec!["The court relocated.".into()],
            entities: vec!["Lantern Bay".into()],
        };
        let results = claim_results(
            "The court relocated after the flood.",
            &[expectation],
            ExpectationKind::Required,
        );

        assert_eq!(results[0].status, ClaimStatus::Unresolved);
        assert!(!results[0].matched);
        assert_eq!(results[0].missing_entities, vec!["Lantern Bay"]);
    }

    #[test]
    fn negated_required_claim_is_clearly_contradicted() {
        let expectation = FactExpectation {
            id: "kingdom-survived".into(),
            claim: "The kingdom survived the Ashen War.".into(),
            aliases: vec!["The kingdom survive the Ashen War.".into()],
            entities: Vec::new(),
        };
        let results = claim_results(
            "The kingdom did not survive the Ashen War.",
            &[expectation],
            ExpectationKind::Required,
        );

        assert_eq!(results[0].status, ClaimStatus::Contradicted);
        assert!(!results[0].matched);
    }

    #[test]
    fn negated_prohibited_claim_is_resolved() {
        let expectation = ProhibitedExpectation {
            id: "kingdom-still-exists".into(),
            claim: "The Ember Kingdom still exists.".into(),
            aliases: Vec::new(),
            entities: vec!["Ember Kingdom".into()],
        };
        let results = claim_results(
            "The Ember Kingdom does not still exist.",
            &[expectation],
            ExpectationKind::Prohibited,
        );

        assert_eq!(results[0].status, ClaimStatus::Resolved);
        assert!(
            prohibited(
                "The Ember Kingdom does not still exist.",
                &[ProhibitedExpectation {
                    id: "kingdom-still-exists".into(),
                    claim: "The Ember Kingdom still exists.".into(),
                    aliases: Vec::new(),
                    entities: vec!["Ember Kingdom".into()],
                }]
            )
            .is_empty()
        );
    }

    #[test]
    fn absent_prohibited_claim_is_resolved_and_present_one_is_contradicted() {
        let expectation = ProhibitedExpectation {
            id: "exhaustive".into(),
            claim: "The chronology is exhaustive.".into(),
            aliases: vec!["The records are complete.".into()],
            entities: Vec::new(),
        };
        let absent = claim_results(
            "The records leave parts of the chronology unknown.",
            std::slice::from_ref(&expectation),
            ExpectationKind::Prohibited,
        );
        let present = claim_results(
            "The chronology is exhaustive.",
            &[expectation],
            ExpectationKind::Prohibited,
        );

        assert_eq!(absent[0].status, ClaimStatus::Resolved);
        assert_eq!(present[0].status, ClaimStatus::Contradicted);
    }
}
