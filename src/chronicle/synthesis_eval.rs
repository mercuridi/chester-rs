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
    cases: Vec<Case>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    question: String,
    expected_route: String,
    required_facts: Vec<String>,
    prohibited_claims: Vec<String>,
    expected_gaps: Vec<String>,
}

#[derive(Debug, Serialize)]
struct CaseReport {
    case: Case,
    answer: String,
    route_correct: bool,
    required_fact_recall: f64,
    prohibited_claims_found: Vec<String>,
    gap_recall: f64,
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
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn coverage(answer: &str, expectations: &[String]) -> f64 {
    if expectations.is_empty() {
        return 1.0;
    }
    let answer = normalise(answer);
    #[allow(clippy::cast_precision_loss)]
    {
        expectations
            .iter()
            .filter(|item| answer.contains(&normalise(item)))
            .count() as f64
            / expectations.len() as f64
    }
}

fn prohibited(answer: &str, expectations: &[String]) -> Vec<String> {
    let answer = normalise(answer);
    expectations
        .iter()
        .filter(|item| answer.contains(&normalise(item)))
        .cloned()
        .collect()
}

fn validate(suite: &Suite) -> Result<()> {
    ensure!(!suite.cases.is_empty(), "Synthesis suite has no cases");
    ensure!((0.0..=1.0).contains(&suite.minimum_required_fact_recall));
    ensure!((0.0..=1.0).contains(&suite.minimum_gap_recall));
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
        ensure!(
            case.required_facts
                .iter()
                .all(|item| !item.trim().is_empty())
        );
        ensure!(
            case.prohibited_claims
                .iter()
                .all(|item| !item.trim().is_empty())
        );
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
        let route_correct = route == Plan::Synthesis {};
        let required_fact_recall = coverage(&answer, &case.required_facts);
        let prohibited_claims_found = prohibited(&answer, &case.prohibited_claims);
        let gap_recall = coverage(&answer, &case.expected_gaps);
        let passed = route_correct
            && required_fact_recall >= suite.minimum_required_fact_recall
            && prohibited_claims_found.len() <= suite.maximum_prohibited_claims
            && gap_recall >= suite.minimum_gap_recall;
        cases.push(CaseReport {
            case,
            answer,
            route_correct,
            required_fact_recall,
            prohibited_claims_found,
            gap_recall,
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
                &["  founded when the river settlements ratified the Ember Compact ".into()]
            ),
            1.0
        );
        assert_eq!(
            prohibited(answer, &["the kingdom still exists".into()]),
            Vec::<String>::new()
        );
        assert_eq!(
            prohibited(
                "The kingdom still exists.",
                &["the kingdom still exists".into()]
            )
            .len(),
            1
        );
    }
}
