use std::time::Instant;

use anyhow::Result;
use tracing::{debug, info};

use crate::chronicle::{
    indexer::AccessScope,
    llm::LanguageModel,
    query::{
        PredeterminedRoute, RouteOperation, StructuredOperation, StructuredPlan,
        StructuredPlanningResult, generate_or_repair_structured_plan, parse_route,
        predetermined_route as query_predetermined_route,
    },
};

use super::chronicle::StructuredStore;

#[derive(Clone, Copy)]
pub(in crate::chronicle::service) enum RetrievalMode {
    Ordinary,
    UnsupportedStructuredQuery,
    ClassificationFailure,
    PlanningFailure,
}

/// A validated answer path. Route selection is complete before route execution begins, keeping
/// structured-query, retrieval, and synthesis policies from leaking into one another.
pub(in crate::chronicle::service) enum AnswerRoute {
    Structured(StructuredPlan),
    Retrieval(RetrievalMode),
    Synthesis,
    Clarification,
    EmptyQuestion,
}

/// The classifier result associated with a single route selection. Keeping this with the
/// selected route prevents callers from having to make a second, potentially divergent,
/// classification request just for reporting.
pub(in crate::chronicle::service) struct RouteSelection {
    pub route: AnswerRoute,
    pub classifier_response: Option<String>,
    pub classifier_error: Option<String>,
    pub classified_operation: Option<RouteOperation>,
}

impl RouteSelection {
    fn predetermined(route: AnswerRoute) -> Self {
        Self {
            route,
            classifier_response: None,
            classifier_error: None,
            classified_operation: None,
        }
    }

    fn classified(route: AnswerRoute, response: String, operation: RouteOperation) -> Self {
        Self {
            route,
            classifier_response: Some(response),
            classifier_error: None,
            classified_operation: Some(operation),
        }
    }

    fn classifier_failure(route: AnswerRoute, response: Option<String>, error: String) -> Self {
        Self {
            route,
            classifier_response: response,
            classifier_error: Some(error),
            classified_operation: None,
        }
    }
}

impl RetrievalMode {
    pub(in crate::chronicle::service) fn prefix(self) -> &'static str {
        match self {
            Self::Ordinary => "",
            Self::UnsupportedStructuredQuery => {
                "An exhaustive count or list is unavailable for this question. "
            }
            Self::ClassificationFailure => {
                "Chronicle couldn't classify this request, so this is a best-effort answer from retrieved notes. "
            }
            Self::PlanningFailure => {
                "I couldn't validate a structured plan for this request, so this is a best-effort answer from retrieved notes. "
            }
        }
    }

    pub(in crate::chronicle::service) fn retrieval_question(self, question: &str) -> String {
        match self {
            Self::Ordinary => question.to_owned(),
            Self::UnsupportedStructuredQuery => format!(
                "{question}\n\nThis query cannot be executed as a structured count or list. Describe only documented examples from the retrieved passages. Do not infer an exhaustive total or claim this is a complete list."
            ),
            Self::ClassificationFailure => format!(
                "{question}\n\nChronicle could not classify this request. Provide a best-effort, non-exhaustive answer using only documented examples from the retrieved passages. Do not infer an exhaustive total or claim this is a complete list."
            ),
            Self::PlanningFailure => format!(
                "{question}\n\nThe structured query planner did not produce a valid plan. Describe only documented examples from the retrieved passages. Do not infer an exhaustive total or claim this is a complete list."
            ),
        }
    }
}

pub(in crate::chronicle::service) async fn select_answer_route(
    llm: &dyn LanguageModel,
    structured_store: &dyn StructuredStore,
    question: &str,
    access: AccessScope,
) -> Result<RouteSelection> {
    let selection_started = Instant::now();
    if let Some(selection) = predetermined_route(question, selection_started) {
        return Ok(selection);
    }

    let classifier_started = Instant::now();
    let (response, operation) = match classify_route(llm, question, classifier_started).await {
        Ok(classification) => classification,
        Err(selection) => return Ok(selection),
    };

    if let Some(selection) = direct_route(&response, operation, classifier_started) {
        return Ok(selection);
    }

    select_structured_route(llm, structured_store, question, access, response, operation).await
}

fn predetermined_route(question: &str, started: Instant) -> Option<RouteSelection> {
    match query_predetermined_route(question)? {
        PredeterminedRoute::EmptyQuestion => {
            emit_route_selection(RouteOperation::Clarify, "routed", "classifier", started);
            Some(RouteSelection::predetermined(AnswerRoute::EmptyQuestion))
        }
        PredeterminedRoute::UnresolvedCollectionReference => {
            emit_route_selection(RouteOperation::Clarify, "routed", "classifier", started);
            Some(RouteSelection::predetermined(AnswerRoute::Clarification))
        }
        PredeterminedRoute::UnsupportedStructuredRequest => {
            debug!(
                question_len = question.chars().count(),
                "Skipping query planner for a definitely unsupported structured request"
            );
            emit_route_selection(RouteOperation::Unsupported, "routed", "classifier", started);
            Some(RouteSelection::predetermined(AnswerRoute::Retrieval(
                RetrievalMode::UnsupportedStructuredQuery,
            )))
        }
    }
}

async fn classify_route(
    llm: &dyn LanguageModel,
    question: &str,
    started: Instant,
) -> std::result::Result<(String, RouteOperation), RouteSelection> {
    let response = match llm.classify_route(question).await {
        Ok(response) => response,
        Err(error) => {
            debug!(question_len = question.chars().count(), %error, "Route classification request failed");
            return Err(classification_failure(started, None, &error));
        }
    };
    debug!(
        question_len = question.chars().count(),
        classifier_response_len = response.chars().count(),
        "Route classifier response received"
    );
    let operation = match parse_route(&response) {
        Ok(operation) => operation,
        Err(error) => {
            debug!(question_len = question.chars().count(), classifier_response_len = response.chars().count(), %error, "Route classification response rejected");
            return Err(classification_failure(started, Some(response), &error));
        }
    };
    debug!(
        question_len = question.chars().count(),
        ?operation,
        "Route classification accepted"
    );
    Ok((response, operation))
}

fn classification_failure(
    started: Instant,
    response: Option<String>,
    error: &anyhow::Error,
) -> RouteSelection {
    tracing::warn!(%error, "Route classification failed; using non-exhaustive retrieval");
    emit_route_selection(
        RouteOperation::Search,
        "classifier_failure",
        "classifier",
        started,
    );
    RouteSelection::classifier_failure(
        AnswerRoute::Retrieval(RetrievalMode::ClassificationFailure),
        response,
        format!("{error:#}"),
    )
}

fn direct_route(
    response: &str,
    operation: RouteOperation,
    started: Instant,
) -> Option<RouteSelection> {
    let route = match operation {
        RouteOperation::Search => AnswerRoute::Retrieval(RetrievalMode::Ordinary),
        RouteOperation::Synthesis => AnswerRoute::Synthesis,
        RouteOperation::Clarify => AnswerRoute::Clarification,
        RouteOperation::Unsupported => {
            AnswerRoute::Retrieval(RetrievalMode::UnsupportedStructuredQuery)
        }
        RouteOperation::Count | RouteOperation::List | RouteOperation::CountMembers => return None,
    };
    emit_route_selection(operation, "routed", "classifier", started);
    Some(RouteSelection::classified(
        route,
        response.to_owned(),
        operation,
    ))
}

async fn select_structured_route(
    llm: &dyn LanguageModel,
    structured_store: &dyn StructuredStore,
    question: &str,
    access: AccessScope,
    response: String,
    operation: RouteOperation,
) -> Result<RouteSelection> {
    let structured_operation = StructuredOperation::try_from(operation)?;
    let generator_started = Instant::now();
    let planning = generate_or_repair_structured_plan(llm, question, structured_operation).await;
    emit_structured_planning_outcome(&planning, operation, question, generator_started);
    let plan = planning.plan;

    let Some(mut plan) = plan else {
        return Ok(RouteSelection::classified(
            AnswerRoute::Retrieval(RetrievalMode::PlanningFailure),
            response,
            operation,
        ));
    };
    if plan.selection().is_some() {
        structured_store
            .resolve_string_or_wikilinks(&mut plan, access)
            .await?;
    }
    debug!(route = ?plan, "Validated Chronicle query plan");
    Ok(RouteSelection::classified(
        AnswerRoute::Structured(plan),
        response,
        operation,
    ))
}

fn emit_structured_planning_outcome(
    planning: &StructuredPlanningResult,
    operation: RouteOperation,
    question: &str,
    started: Instant,
) {
    if let Some(error) = planning.generation_error.as_deref() {
        debug!(question_len = question.chars().count(), %error, ?operation, "Chronicle structured query generation failed");
        tracing::warn!(%error, ?operation, "Structured query planning failed; using non-exhaustive retrieval");
        emit_route_selection(
            operation,
            "generation_failure",
            "structured_generator",
            started,
        );
    } else if let Some(error) = planning
        .repair_validation_error
        .as_deref()
        .or(planning.repair_error.as_deref())
    {
        debug!(question_len = question.chars().count(), %error, "Chronicle structured query repair rejected");
        tracing::warn!(%error, "Structured query planning failed after retry; using non-exhaustive retrieval");
        emit_route_selection(operation, "repair_failure", "structured_repair", started);
    } else if planning.repair_response.is_some() {
        emit_route_selection(operation, "repaired", "structured_repair", started);
    } else {
        emit_route_selection(operation, "routed", "structured_generator", started);
    }
}

/// Emits low-cardinality production telemetry for every completed routing stage. Detailed model
/// inputs and outputs are intentionally kept in the adjacent debug events.
fn emit_route_selection(
    operation: RouteOperation,
    outcome: &'static str,
    stage: &'static str,
    started: Instant,
) {
    info!(
        operation = operation.as_str(),
        outcome,
        stage,
        duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        structured = operation.is_structured(),
        "Chronicle route selection"
    );
}
