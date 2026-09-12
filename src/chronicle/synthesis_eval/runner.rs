//! Model-backed bounded-synthesis evaluation with deterministic rubric scoring.
use crate::chronicle::{
    config::Config,
    indexer::{db::IndexerDb, embedder::Embedder, retriever::runtime::Retriever, service::Indexer},
    llm::{LanguageModel, Llm},
    query::plan::RouteOperation,
    runtime::GpuRuntime,
    service::{Chronicle, ChronicleDependencies, EffectiveRoute},
    transcription::service::TranscriptionService,
};
use anyhow::{Context, Result, anyhow, ensure};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::{
    fs::{OpenOptions, create_dir_all},
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Suite {
    name: String,
    minimum_required_fact_recall: f64,
    maximum_prohibited_claims: usize,
    minimum_gap_recall: f64,
    #[serde(default = "default_minimum_core_recall")]
    minimum_core_recall: f64,
    #[serde(default)]
    minimum_supporting_recall: f64,
    #[serde(default)]
    minimum_caveat_recall: f64,
    #[serde(default)]
    maximum_unsupported_major_causal_claims: usize,
    topology: Topology,
    safety: Safety,
    cases: Vec<Case>,
}

fn default_minimum_core_recall() -> f64 {
    0.8
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
#[allow(clippy::struct_field_names)]
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
    #[serde(default = "default_core_category")]
    category: ClaimCategory,
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
    #[serde(default = "default_caveat_category")]
    category: ClaimCategory,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ClaimCategory {
    Core,
    Supporting,
    Caveat,
}

fn default_core_category() -> ClaimCategory {
    ClaimCategory::Core
}

fn default_caveat_category() -> ClaimCategory {
    ClaimCategory::Caveat
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
        #[serde(default)]
        category: Option<ClaimCategory>,
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
                category: ClaimCategory::Core,
            }),
            RawExpectation::Structured {
                id,
                claim,
                aliases,
                entities,
                category,
            } => Ok(FactExpectation {
                id,
                claim,
                aliases,
                entities,
                category: category.unwrap_or(ClaimCategory::Core),
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
                category: _,
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
                category: ClaimCategory::Caveat,
            }),
            RawExpectation::Structured {
                id,
                claim,
                aliases,
                entities,
                category,
            } => Ok(GapExpectation {
                id,
                claim,
                aliases,
                entities,
                category: category.unwrap_or(ClaimCategory::Caveat),
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
    category: Option<ClaimCategory>,
    matched: bool,
    status: ClaimStatus,
    matched_by: Option<String>,
    missing_entities: Vec<String>,
    judge_verdict: Option<JudgeVerdict>,
    judge_confidence: Option<f64>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ClaimStatus {
    Resolved,
    Unresolved,
    Contradicted,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum JudgeVerdict {
    Entailed,
    Contradicted,
    Unknown,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JudgeClaim {
    id: String,
    verdict: JudgeVerdict,
    confidence: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JudgeOutput {
    claims: Vec<JudgeClaim>,
    unsupported_causal_claims: Vec<UnsupportedCausalClaim>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UnsupportedCausalClaim {
    claim: String,
    confidence: f64,
}

#[derive(Debug, Serialize)]
struct JudgeTarget {
    id: String,
    kind: String,
    claim: String,
    aliases: Vec<String>,
    entities: Vec<String>,
    missing_entities: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum JudgeStatus {
    NotNeeded,
    Succeeded,
    Error,
}

#[derive(Debug, Serialize)]
struct JudgeMetadata {
    status: JudgeStatus,
    target_count: usize,
    judged_claim_count: usize,
    attempts: usize,
    retry_count: usize,
    unsupported_causal_claim_count: usize,
    error: Option<String>,
}

struct JudgeEvaluation {
    claims: Vec<JudgeClaim>,
    unsupported_causal_claims: Vec<String>,
    metadata: JudgeMetadata,
}

struct SynthesisJudge {
    model: std::sync::Arc<dyn LanguageModel>,
    max_attempts: usize,
}

fn judge_prompt(
    question: &str,
    answer: &str,
    targets: &[JudgeTarget],
    invalid_response: Option<&str>,
) -> String {
    let targets = serde_json::to_string(targets).unwrap_or_default();
    let mut prompt = format!(
        "You are a strict evaluator of a synthesis answer. Determine whether each unresolved rubric claim is entailed by the answer, contradicted by the answer, or unknown. Accept faithful paraphrases, but do not infer facts that the answer does not state. Use the entity requirements as hard constraints. Also flag any major causal claim in the answer that is not directly supported by the supplied rubric claims. Return JSON only, with exactly this schema: {{\"claims\":[{{\"id\":\"target id\",\"verdict\":\"entailed|contradicted|unknown\",\"confidence\":0.0}}],\"unsupported_causal_claims\":[{{\"claim\":\"short claim\",\"confidence\":0.0}}]}}. Include exactly one result for every supplied target, preserving each target id. Use an empty unsupported_causal_claims array when none are present. Confidence must be a JSON number from 0.0 to 1.0. Do not include markdown, explanations, or additional fields.\n\nQuestion:\n{question}\n\nAnswer:\n{answer}\n\nUnresolved targets:\n{targets}\n"
    );
    if let Some(invalid_response) = invalid_response {
        prompt.push_str(
            "\nYour previous response was invalid. Correct it and return only the required JSON object. Previous response:\n",
        );
        prompt.push_str(invalid_response);
    }
    prompt
}

fn parse_judge_output(response: &str, targets: &[JudgeTarget]) -> Result<JudgeOutput> {
    let mut output: JudgeOutput =
        serde_json::from_str(response).context("judge response was not valid strict JSON")?;
    let expected = targets
        .iter()
        .map(|target| target.id.as_str())
        .collect::<std::collections::HashSet<_>>();
    let mut seen = std::collections::HashSet::new();
    for claim in &output.claims {
        ensure!(
            expected.contains(claim.id.as_str()),
            "judge returned unknown claim ID: {}",
            claim.id
        );
        ensure!(
            seen.insert(claim.id.as_str()),
            "judge returned duplicate claim ID: {}",
            claim.id
        );
        ensure!(
            claim.confidence.is_finite() && (0.0..=1.0).contains(&claim.confidence),
            "judge confidence for {} must be between 0.0 and 1.0",
            claim.id
        );
    }
    for causal_claim in &output.unsupported_causal_claims {
        ensure!(
            causal_claim.confidence.is_finite() && (0.0..=1.0).contains(&causal_claim.confidence),
            "unsupported causal claim confidence must be between 0.0 and 1.0"
        );
        ensure!(
            !causal_claim.claim.trim().is_empty(),
            "unsupported causal claim cannot be empty"
        );
    }
    // A local model can occasionally omit a target while still returning a
    // useful, otherwise-valid evaluation. Treat omitted targets
    // conservatively as unknown instead of discarding the whole judge result.
    // Unknown claims have zero confidence, so apply_judge_results will leave
    // the deterministic rubric result unchanged.
    let present_ids = seen
        .iter()
        .map(|id| (*id).to_owned())
        .collect::<std::collections::HashSet<_>>();
    for target in targets {
        if !present_ids.contains(&target.id) {
            output.claims.push(JudgeClaim {
                id: target.id.clone(),
                verdict: JudgeVerdict::Unknown,
                confidence: 0.0,
            });
        }
    }
    Ok(output)
}

impl SynthesisJudge {
    fn new(model: std::sync::Arc<dyn LanguageModel>) -> Self {
        Self {
            model,
            max_attempts: 2,
        }
    }

    async fn judge(
        &self,
        question: &str,
        answer: &str,
        targets: &[JudgeTarget],
    ) -> Result<JudgeEvaluation> {
        ensure!(
            !targets.is_empty(),
            "Synthesis judge requires at least one target"
        );
        let prompt = judge_prompt(question, answer, targets, None);
        let mut attempts = 0;
        let mut retry_count = 0;
        let mut last_error = None;
        let mut previous_response: Option<String> = None;
        while attempts < self.max_attempts {
            attempts += 1;
            let request = match &previous_response {
                Some(response) => judge_prompt(question, answer, targets, Some(response)),
                None => prompt.clone(),
            };
            let response = match self.model.generate(&request).await {
                Ok(response) => response,
                Err(error) => {
                    last_error = Some(error.to_string());
                    if attempts < self.max_attempts {
                        retry_count += 1;
                    }
                    continue;
                }
            };
            match parse_judge_output(&response, targets) {
                Ok(output) => {
                    let unsupported_causal_claims = output
                        .unsupported_causal_claims
                        .iter()
                        .filter(|claim| claim.confidence >= 0.75)
                        .map(|claim| claim.claim.clone())
                        .collect::<Vec<_>>();
                    let metadata = JudgeMetadata {
                        status: JudgeStatus::Succeeded,
                        target_count: targets.len(),
                        judged_claim_count: output.claims.len(),
                        attempts,
                        retry_count,
                        unsupported_causal_claim_count: unsupported_causal_claims.len(),
                        error: None,
                    };
                    return Ok(JudgeEvaluation {
                        claims: output.claims,
                        unsupported_causal_claims,
                        metadata,
                    });
                }
                Err(error) => {
                    last_error = Some(error.to_string());
                    previous_response = Some(response);
                    if attempts < self.max_attempts {
                        retry_count += 1;
                    }
                }
            }
        }
        Err(anyhow!(
            "Synthesis judge failed after {attempts} attempts: {}",
            last_error.unwrap_or_else(|| "no response".into())
        ))
    }
}

#[derive(Debug, Serialize)]
struct CaseReport {
    case: Case,
    expected_operation: RouteOperation,
    classifier_response: Option<String>,
    classifier_error: Option<String>,
    classified_operation: Option<RouteOperation>,
    effective_route: EffectiveRoute,
    route_correct: bool,
    answer: String,
    synthesis_diagnostics: Option<super::super::service::SynthesisDiagnostics>,
    required_fact_recall: f64,
    core_recall: f64,
    supporting_recall: f64,
    caveat_recall: f64,
    prohibited_claims_found: Vec<String>,
    contradictions: usize,
    unsupported_major_causal_claims: Vec<String>,
    gap_recall: f64,
    required_fact_results: Vec<ClaimResult>,
    prohibited_claim_results: Vec<ClaimResult>,
    gap_results: Vec<ClaimResult>,
    judge: JudgeMetadata,
    passed: bool,
}

#[derive(Serialize)]
struct Report {
    suite: String,
    model: String,
    minimum_required_fact_recall: f64,
    maximum_prohibited_claims: usize,
    minimum_gap_recall: f64,
    minimum_core_recall: f64,
    minimum_supporting_recall: f64,
    minimum_caveat_recall: f64,
    maximum_unsupported_major_causal_claims: usize,
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
    fn category(&self) -> Option<ClaimCategory>;
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

    fn category(&self) -> Option<ClaimCategory> {
        Some(self.category)
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

    fn category(&self) -> Option<ClaimCategory> {
        None
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

    fn category(&self) -> Option<ClaimCategory> {
        Some(self.category)
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
                category: expectation.category(),
                matched,
                status,
                matched_by: found.matched_by,
                missing_entities: found.missing_entities,
                judge_verdict: None,
                judge_confidence: None,
            }
        })
        .collect()
}

fn resolved_coverage(results: &[ClaimResult]) -> f64 {
    if results.is_empty() {
        return 1.0;
    }
    #[allow(clippy::cast_precision_loss)]
    {
        results
            .iter()
            .filter(|result| result.status == ClaimStatus::Resolved)
            .count() as f64
            / results.len() as f64
    }
}

fn category_coverage(
    required_facts: &[ClaimResult],
    gaps: &[ClaimResult],
    category: ClaimCategory,
) -> f64 {
    let claims = required_facts
        .iter()
        .chain(gaps.iter())
        .filter(|result| result.category == Some(category))
        .collect::<Vec<_>>();
    if claims.is_empty() {
        return 1.0;
    }
    #[allow(clippy::cast_precision_loss)]
    {
        claims
            .iter()
            .filter(|result| result.status == ClaimStatus::Resolved)
            .count() as f64
            / claims.len() as f64
    }
}

fn contradiction_count(
    required_facts: &[ClaimResult],
    prohibited_claims: &[ClaimResult],
    gaps: &[ClaimResult],
) -> usize {
    required_facts
        .iter()
        .chain(prohibited_claims.iter())
        .chain(gaps.iter())
        .filter(|result| result.status == ClaimStatus::Contradicted)
        .count()
}

fn unresolved_targets<T: Expectation>(
    results: &[ClaimResult],
    expectations: &[T],
    kind: &str,
) -> Vec<JudgeTarget> {
    results
        .iter()
        .filter(|result| result.status == ClaimStatus::Unresolved)
        .filter_map(|result| {
            expectations
                .iter()
                .find(|expectation| expectation.id() == result.id)
                .map(|expectation| JudgeTarget {
                    id: format!("{kind}:{}", expectation.id()),
                    kind: kind.to_owned(),
                    claim: expectation.claim().to_owned(),
                    aliases: expectation.aliases().to_owned(),
                    entities: expectation.entities().to_owned(),
                    missing_entities: result.missing_entities.clone(),
                })
        })
        .collect()
}

fn apply_judge_results(
    results: &mut [ClaimResult],
    kind: &str,
    judged: &[JudgeClaim],
) -> Result<()> {
    for claim in judged {
        if !claim.id.starts_with(&format!("{kind}:")) {
            continue;
        }
        let expected_id = claim
            .id
            .strip_prefix(&format!("{kind}:"))
            .with_context(|| format!("judge returned wrong claim namespace: {}", claim.id))?;
        let result = results
            .iter_mut()
            .find(|result| result.id == expected_id)
            .with_context(|| format!("judge returned unknown {kind} result: {expected_id}"))?;
        result.judge_verdict = Some(claim.verdict);
        result.judge_confidence = Some(claim.confidence);
        if claim.confidence >= 0.75 {
            result.status = match claim.verdict {
                JudgeVerdict::Entailed => ClaimStatus::Resolved,
                JudgeVerdict::Contradicted => ClaimStatus::Contradicted,
                JudgeVerdict::Unknown => ClaimStatus::Unresolved,
            };
            result.matched = claim.verdict == JudgeVerdict::Entailed;
            result.matched_by = Some("judge".into());
        }
    }
    Ok(())
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
    ensure!((0.0..=1.0).contains(&suite.minimum_core_recall));
    ensure!((0.0..=1.0).contains(&suite.minimum_supporting_recall));
    ensure!((0.0..=1.0).contains(&suite.minimum_caveat_recall));
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

fn create_report_file(
    requested: Option<&Path>,
    log_dir: &Path,
) -> Result<(std::fs::File, PathBuf)> {
    let path = requested.map_or_else(
        || {
            log_dir.join("evaluation").join(format!(
                "chronicle-synthesis-report-{}.json",
                Utc::now().format("%Y%m%d-%H%M%S")
            ))
        },
        Path::to_path_buf,
    );
    if requested.is_none() {
        let directory = log_dir.join("evaluation");
        create_dir_all(&directory).with_context(|| {
            format!(
                "Failed to create report directory at {}",
                directory.display()
            )
        })?;
    }
    ensure!(!path.exists(), "Report path must be a new file");
    Ok((
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?,
        path,
    ))
}

struct CaseClaimResults {
    required_facts: Vec<ClaimResult>,
    prohibited_claims: Vec<ClaimResult>,
    gaps: Vec<ClaimResult>,
}

struct JudgeOutcome {
    metadata: JudgeMetadata,
    failed: bool,
    unsupported_major_causal_claims: Vec<String>,
}

#[derive(Clone, Copy)]
struct EvaluationThresholds {
    minimum_required_fact_recall: f64,
    maximum_prohibited_claims: usize,
    minimum_gap_recall: f64,
    minimum_core_recall: f64,
    minimum_supporting_recall: f64,
    minimum_caveat_recall: f64,
    maximum_unsupported_major_causal_claims: usize,
}

impl From<&Suite> for EvaluationThresholds {
    fn from(suite: &Suite) -> Self {
        Self {
            minimum_required_fact_recall: suite.minimum_required_fact_recall,
            maximum_prohibited_claims: suite.maximum_prohibited_claims,
            minimum_gap_recall: suite.minimum_gap_recall,
            minimum_core_recall: suite.minimum_core_recall,
            minimum_supporting_recall: suite.minimum_supporting_recall,
            minimum_caveat_recall: suite.minimum_caveat_recall,
            maximum_unsupported_major_causal_claims: suite.maximum_unsupported_major_causal_claims,
        }
    }
}

fn initial_claim_results(case: &Case, answer: &str) -> CaseClaimResults {
    CaseClaimResults {
        required_facts: claim_results(answer, &case.required_facts, ExpectationKind::Required),
        prohibited_claims: claim_results(
            answer,
            &case.prohibited_claims,
            ExpectationKind::Prohibited,
        ),
        gaps: claim_results(answer, &case.expected_gaps, ExpectationKind::Gap),
    }
}

fn judge_targets(case: &Case, results: &CaseClaimResults) -> Vec<JudgeTarget> {
    let mut targets = unresolved_targets(
        &results.required_facts,
        &case.required_facts,
        "required_fact",
    );
    targets.extend(unresolved_targets(
        &results.prohibited_claims,
        &case.prohibited_claims,
        "prohibited_claim",
    ));
    targets.extend(unresolved_targets(
        &results.gaps,
        &case.expected_gaps,
        "gap",
    ));
    targets
}

fn judge_not_needed() -> JudgeOutcome {
    JudgeOutcome {
        metadata: JudgeMetadata {
            status: JudgeStatus::NotNeeded,
            target_count: 0,
            judged_claim_count: 0,
            attempts: 0,
            retry_count: 0,
            unsupported_causal_claim_count: 0,
            error: None,
        },
        failed: false,
        unsupported_major_causal_claims: Vec::new(),
    }
}

async fn judge_unresolved_claims(
    judge: &SynthesisJudge,
    question: &str,
    answer: &str,
    targets: &[JudgeTarget],
    results: &mut CaseClaimResults,
) -> JudgeOutcome {
    if targets.is_empty() {
        return judge_not_needed();
    }
    match judge.judge(question, answer, targets).await {
        Ok(JudgeEvaluation {
            claims,
            unsupported_causal_claims,
            metadata,
        }) => {
            let applied =
                apply_judge_results(&mut results.required_facts, "required_fact", &claims)
                    .and_then(|()| {
                        apply_judge_results(
                            &mut results.prohibited_claims,
                            "prohibited_claim",
                            &claims,
                        )
                    })
                    .and_then(|()| apply_judge_results(&mut results.gaps, "gap", &claims));
            match applied {
                Ok(()) => JudgeOutcome {
                    metadata,
                    failed: false,
                    unsupported_major_causal_claims: unsupported_causal_claims,
                },
                Err(error) => JudgeOutcome {
                    metadata: JudgeMetadata {
                        status: JudgeStatus::Error,
                        target_count: targets.len(),
                        judged_claim_count: claims.len(),
                        attempts: metadata.attempts,
                        retry_count: metadata.retry_count,
                        unsupported_causal_claim_count: metadata.unsupported_causal_claim_count,
                        error: Some(error.to_string()),
                    },
                    failed: true,
                    unsupported_major_causal_claims: Vec::new(),
                },
            }
        }
        Err(error) => JudgeOutcome {
            metadata: JudgeMetadata {
                status: JudgeStatus::Error,
                target_count: targets.len(),
                judged_claim_count: 0,
                attempts: judge.max_attempts,
                retry_count: judge.max_attempts.saturating_sub(1),
                unsupported_causal_claim_count: 0,
                error: Some(error.to_string()),
            },
            failed: true,
            unsupported_major_causal_claims: Vec::new(),
        },
    }
}

async fn evaluate_case(
    case: Case,
    chronicle: &Chronicle,
    judge: &SynthesisJudge,
    thresholds: EvaluationThresholds,
) -> Result<CaseReport> {
    let runtime_answer = chronicle.ask_with_metadata(&case.question).await?;
    let route_correct = runtime_answer.effective_route == EffectiveRoute::Synthesis;
    let classifier_response = runtime_answer.classifier_response;
    let classifier_error = runtime_answer.classifier_error;
    let classified_operation = runtime_answer.classified_operation;
    let effective_route = runtime_answer.effective_route;
    let synthesis_diagnostics = runtime_answer.synthesis_diagnostics;
    let answer = runtime_answer.reply;
    let mut results = initial_claim_results(&case, &answer);
    let targets = judge_targets(&case, &results);
    let judge =
        judge_unresolved_claims(judge, &case.question, &answer, &targets, &mut results).await;
    let required_fact_recall = resolved_coverage(&results.required_facts);
    let prohibited_claims_found = results
        .prohibited_claims
        .iter()
        .filter(|result| result.status == ClaimStatus::Contradicted)
        .map(|result| result.claim.clone())
        .collect::<Vec<_>>();
    let gap_recall = resolved_coverage(&results.gaps);
    let core_recall =
        category_coverage(&results.required_facts, &results.gaps, ClaimCategory::Core);
    let supporting_recall = category_coverage(
        &results.required_facts,
        &results.gaps,
        ClaimCategory::Supporting,
    );
    let caveat_recall = category_coverage(
        &results.required_facts,
        &results.gaps,
        ClaimCategory::Caveat,
    );
    let contradictions = contradiction_count(
        &results.required_facts,
        &results.prohibited_claims,
        &results.gaps,
    );
    let passed = route_correct
        && !judge.failed
        && required_fact_recall >= thresholds.minimum_required_fact_recall
        && prohibited_claims_found.len() <= thresholds.maximum_prohibited_claims
        && gap_recall >= thresholds.minimum_gap_recall
        && core_recall >= thresholds.minimum_core_recall
        && supporting_recall >= thresholds.minimum_supporting_recall
        && caveat_recall >= thresholds.minimum_caveat_recall
        && contradictions == 0
        && judge.unsupported_major_causal_claims.len()
            <= thresholds.maximum_unsupported_major_causal_claims;
    Ok(CaseReport {
        case,
        expected_operation: RouteOperation::Synthesis,
        classifier_response,
        classifier_error,
        classified_operation,
        effective_route,
        route_correct,
        answer,
        synthesis_diagnostics,
        required_fact_recall,
        core_recall,
        supporting_recall,
        caveat_recall,
        prohibited_claims_found,
        contradictions,
        unsupported_major_causal_claims: judge.unsupported_major_causal_claims,
        gap_recall,
        required_fact_results: results.required_facts,
        prohibited_claim_results: results.prohibited_claims,
        gap_results: results.gaps,
        judge: judge.metadata,
        passed,
    })
}

pub async fn run(
    suite_path: &Path,
    requested_report: Option<&Path>,
    paths: &crate::chronicle::config::AppPaths,
) -> Result<()> {
    let suite: Suite = toml::from_str(&std::fs::read_to_string(suite_path)?)?;
    validate(&suite)?;
    let thresholds = EvaluationThresholds::from(&suite);
    let config = Config::load(paths.clone())?;
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
    let indexer = Indexer::new(
        corpus,
        database,
        Embedder::load(candle_core::Device::Cpu)?,
        config.chronicle.indexing.max_chunk_tokens,
        config.chronicle.indexing.chunk_overlap_tokens,
    );
    indexer.index().await?;
    let (database, _) = indexer.into_parts();
    let runtime = GpuRuntime::new();
    let llm = Llm::new(&config.chronicle.llm, runtime.clone());
    let model = format!(
        "{}@{} / {}",
        config.chronicle.llm.model.repo,
        config.chronicle.llm.model.revision,
        config.chronicle.llm.model.file
    );
    let retriever = Arc::new(Retriever::new(database.clone()));
    let llm = Arc::new(llm);
    let chronicle = Chronicle::new(
        config.chronicle.retrieval,
        config.chronicle.synthesis,
        config.chronicle.llm.generation.clone(),
        ChronicleDependencies {
            retriever,
            structured_store: Arc::new(database),
            llm: llm.clone(),
            runtime: runtime.clone(),
            transcription: TranscriptionService::new(runtime),
        },
    );
    chronicle.start_llm().await?;
    let judge = SynthesisJudge::new(llm.clone());
    let mut cases = Vec::new();
    for case in suite.cases {
        cases.push(evaluate_case(case, &chronicle, &judge, thresholds).await?);
    }
    chronicle.stop_llm().await?;
    let passed = cases.iter().all(|case| case.passed);
    let report = Report {
        suite: suite.name,
        model,
        minimum_required_fact_recall: thresholds.minimum_required_fact_recall,
        maximum_prohibited_claims: thresholds.maximum_prohibited_claims,
        minimum_gap_recall: thresholds.minimum_gap_recall,
        minimum_core_recall: thresholds.minimum_core_recall,
        minimum_supporting_recall: thresholds.minimum_supporting_recall,
        minimum_caveat_recall: thresholds.minimum_caveat_recall,
        maximum_unsupported_major_causal_claims: thresholds.maximum_unsupported_major_causal_claims,
        passed,
        cases,
    };
    let (file, path) = create_report_file(requested_report, &paths.log_dir)?;
    serde_json::to_writer_pretty(file, &report)?;
    ensure!(
        passed,
        "Synthesis evaluation failed; see {}",
        path.display()
    );
    Ok(())
}

#[cfg(test)]
#[allow(clippy::float_cmp, clippy::unwrap_used)]
#[path = "tests.rs"]
mod tests;
