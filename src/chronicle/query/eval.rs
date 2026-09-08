//! Separate structured-query evaluation; leaves the retrieval baseline unchanged.
use super::{plan::Plan, planner};
use crate::chronicle::{
    config::Config,
    indexer::{
        db::repository::{IndexerDb, StructuredResult},
        scanner,
    },
    llm::Llm,
    runtime::GpuRuntime,
};
use anyhow::{Context, Result, ensure};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Suite {
    name: String,
    minimum_planner_accuracy: f64,
    cases: Vec<Case>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    question: String,
    #[serde(default = "default_intent_family")]
    intent_family: String,
    plan: Plan,
    expected_total: Option<i64>,
    expected_ids: Option<Vec<String>>,
}
fn default_intent_family() -> String {
    "unclassified".into()
}
#[derive(Serialize)]
struct CaseReport {
    case: Case,
    result: Option<StructuredResult>,
    executor_correct: bool,
    planner_responses: Vec<String>,
    actual_plan: Option<Plan>,
    planner_error: Option<String>,
    planner_correct: Option<bool>,
}
#[derive(Serialize)]
struct Report {
    suite: String,
    fixture_sha256: String,
    planner_model: Option<String>,
    minimum_planner_accuracy: f64,
    planner_accuracy: Option<f64>,
    intent_family_accuracy: BTreeMap<String, IntentFamilyAccuracy>,
    passed: bool,
    cases: Vec<CaseReport>,
}

#[derive(Serialize)]
struct IntentFamilyAccuracy {
    cases: usize,
    correct: usize,
    accuracy: Option<f64>,
}

fn family_accuracy(
    cases: &[CaseReport],
    test_planner: bool,
) -> BTreeMap<String, IntentFamilyAccuracy> {
    let mut counts = BTreeMap::<String, (usize, usize)>::new();
    for case in cases {
        let entry = counts.entry(case.case.intent_family.clone()).or_default();
        entry.0 += 1;
        if case.planner_correct == Some(true) {
            entry.1 += 1;
        }
    }
    counts
        .into_iter()
        .map(|(family, (cases, correct))| {
            (
                family,
                IntentFamilyAccuracy {
                    cases,
                    correct,
                    accuracy: test_planner.then(|| correct as f64 / cases as f64),
                },
            )
        })
        .collect()
}

async fn fixture_database(suite_path: &Path) -> Result<(tempfile::TempDir, IndexerDb, String)> {
    use sha2::{Digest, Sha256};
    let temp = tempfile::tempdir()?;
    let db = IndexerDb::open(&format!(
        "sqlite://{}",
        temp.path().join("chronicle.sqlite3").display()
    ))
    .await?;
    let (notes, _) = scanner::scan_directory_with_stats(
        suite_path
            .parent()
            .context("Missing suite directory")?
            .join("corpus"),
    )?;
    let mut hash = Sha256::new();
    hash.update(std::fs::read(suite_path)?);
    for note in notes {
        hash.update(note.metadata.id.as_bytes());
        hash.update(note.content_hash.as_bytes());
        // Structured execution requires no chunks, vectors or embedding model.
        db.replace_note(
            &note.path.to_string_lossy(),
            &note.content_hash,
            &[],
            &[],
            &note.metadata,
        )
        .await?;
    }
    Ok((temp, db, hex::encode(hash.finalize())))
}

fn validate(suite: &Suite) -> Result<()> {
    ensure!(!suite.cases.is_empty(), "Suite has no cases");
    ensure!(
        (0.0..=1.0).contains(&suite.minimum_planner_accuracy),
        "Invalid planner accuracy threshold"
    );
    let mut ids = std::collections::HashSet::new();
    for case in &suite.cases {
        ensure!(
            !case.id.is_empty() && ids.insert(&case.id),
            "Duplicate or empty case ID"
        );
        ensure!(
            !case.question.trim().is_empty(),
            "Empty evaluation question"
        );
        case.plan.validate()?;
        let structured = case.plan.selection().is_some();
        ensure!(
            structured == case.expected_total.is_some(),
            "Structured cases require an expected total"
        );
        ensure!(
            case.expected_total.is_none_or(|n| n >= 0),
            "Negative expected count"
        );
        ensure!(
            matches!(case.plan, Plan::List { .. }) == case.expected_ids.is_some(),
            "List cases require expected IDs"
        );
    }
    Ok(())
}

async fn evaluate(
    case: Case,
    db: &IndexerDb,
    llm: Option<&Llm>,
    runtime: &GpuRuntime,
) -> Result<CaseReport> {
    let result = if case.plan.selection().is_some() {
        Some(db.execute_plan(&case.plan).await?)
    } else {
        None
    };
    let executor_correct = result.as_ref().is_none_or(|r| {
        Some(r.total) == case.expected_total
            && case.expected_ids.as_ref().is_none_or(|ids| {
                r.notes.iter().map(|n| &n.id).collect::<Vec<_>>() == ids.iter().collect::<Vec<_>>()
            })
    });
    let mut report = CaseReport {
        case,
        result,
        executor_correct,
        planner_responses: Vec::new(),
        actual_plan: None,
        planner_error: None,
        planner_correct: None,
    };
    if let Some(llm) = llm {
        let _lease = runtime.acquire_inference()?;
        match llm.generate_plan(&report.case.question).await {
            Ok(response) => {
                report.planner_responses.push(response.clone());
                match planner::parse_for_question(&report.case.question, &response) {
                    Ok(mut plan) => {
                        db.resolve_string_or_wikilinks(&mut plan).await?;
                        report.planner_correct = Some(plan == report.case.plan);
                        report.actual_plan = Some(plan);
                    }
                    Err(initial_error) => {
                        match llm
                            .repair_plan(
                                &report.case.question,
                                &response,
                                &initial_error.to_string(),
                            )
                            .await
                        {
                            Ok(retry_response) => {
                                report.planner_responses.push(retry_response.clone());
                                match planner::parse_for_question(
                                    &report.case.question,
                                    &retry_response,
                                ) {
                                    Ok(mut plan) => {
                                        db.resolve_string_or_wikilinks(&mut plan).await?;
                                        report.planner_correct = Some(plan == report.case.plan);
                                        report.actual_plan = Some(plan);
                                    }
                                    Err(retry_error) => {
                                        report.planner_correct = Some(false);
                                        report.planner_error = Some(format!(
                                            "Initial planner response rejected: {initial_error:#}\nPlanner retry response rejected: {retry_error:#}"
                                        ));
                                    }
                                }
                            }
                            Err(retry_error) => {
                                report.planner_correct = Some(false);
                                report.planner_error = Some(format!(
                                    "Initial planner response rejected: {initial_error:#}\nPlanner retry failed: {retry_error:#}"
                                ));
                            }
                        }
                    }
                }
            }
            Err(error) => {
                report.planner_correct = Some(false);
                report.planner_error = Some(format!("{error:#}"));
            }
        }
    }
    Ok(report)
}

fn create_report_file(requested_path: Option<&Path>) -> Result<(File, PathBuf)> {
    if let Some(path) = requested_path {
        ensure!(!path.exists(), "Report path must be a new file");
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .context("Report path must be a new file")?;
        return Ok((file, path.to_owned()));
    }

    let directory = std::env::current_dir().context("Failed to determine report directory")?;
    let timestamp = Utc::now().format("%Y%m%d-%H%M%S");
    let mut suffix = 0_u64;
    loop {
        let name = if suffix == 0 {
            format!("chronicle-query-report-{timestamp}.json")
        } else {
            format!("chronicle-query-report-{timestamp}-{suffix}.json")
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

pub async fn run(
    suite_path: &Path,
    requested_report_path: Option<&Path>,
    test_planner: bool,
) -> Result<()> {
    if let Some(path) = requested_report_path {
        ensure!(!path.exists(), "Report path must be a new file");
    }
    let suite: Suite = toml::from_str(&std::fs::read_to_string(suite_path)?)?;
    validate(&suite)?;
    let (_temp, db, fixture_sha256) = fixture_database(suite_path).await?;
    let runtime = GpuRuntime::new();
    let mut planner_model = None;
    let llm = if test_planner {
        let config =
            Config::load(Path::new(env!("CARGO_MANIFEST_DIR")).join(".chronicle/config.toml"))?;
        planner_model = Some(format!(
            "{}@{} / {}",
            config.chronicle.llm_repo,
            config.chronicle.llm_revision,
            config.chronicle.llm_model_file
        ));
        let llm = Llm::new(&config.chronicle, runtime.clone());
        llm.load().await?;
        Some(llm)
    } else {
        None
    };
    let mut cases = Vec::new();
    for case in suite.cases {
        cases.push(evaluate(case, &db, llm.as_ref(), &runtime).await?);
    }
    if let Some(llm) = llm {
        llm.unload().await?;
    }
    #[allow(clippy::cast_precision_loss)]
    let planner_accuracy = test_planner.then(|| {
        cases
            .iter()
            .filter(|c| c.planner_correct == Some(true))
            .count() as f64
            / cases.len() as f64
    });
    // Reject any unsupported/ambiguous question incorrectly accepted for SQL,
    // even if aggregate planner accuracy meets the threshold.
    let unsafe_acceptance = cases.iter().any(|c| {
        c.case.plan.selection().is_none()
            && c.actual_plan
                .as_ref()
                .is_some_and(|p| p.selection().is_some())
    });
    let passed = cases.iter().all(|c| c.executor_correct)
        && !unsafe_acceptance
        && planner_accuracy.is_none_or(|a| a >= suite.minimum_planner_accuracy);
    let report = Report {
        suite: suite.name,
        fixture_sha256,
        planner_model,
        minimum_planner_accuracy: suite.minimum_planner_accuracy,
        planner_accuracy,
        intent_family_accuracy: family_accuracy(&cases, test_planner),
        passed,
        cases,
    };
    let (file, report_path) = create_report_file(requested_report_path)?;
    serde_json::to_writer_pretty(file, &report)?;
    tracing::info!(passed, ?planner_accuracy, report = %report_path.display(), "Structured query evaluation complete");
    ensure!(
        passed,
        "Structured query evaluation failed; see {}",
        report_path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn fixed_suite_executes_without_model_or_vectors() -> Result<()> {
        let root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/chronicle-query/suite.toml");
        let suite: Suite = toml::from_str(&std::fs::read_to_string(&root)?)?;
        validate(&suite)?;
        let (_temp, db, _) = fixture_database(&root).await?;
        for case in suite.cases {
            let report = evaluate(case, &db, None, &GpuRuntime::new()).await?;
            ensure!(
                report.executor_correct,
                "Incorrect executor result for {}",
                report.case.id
            );
        }
        Ok(())
    }
}
