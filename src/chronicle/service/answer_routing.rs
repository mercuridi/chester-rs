use anyhow::{Result, ensure};
use std::time::Instant;
use tracing::{debug, info};

use super::super::{
    llm::LanguageModel,
    query::{
        classifier,
        plan::{Plan, RouteOperation},
        planner,
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
    Structured(Plan),
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
) -> Result<RouteSelection> {
    let selection_started = Instant::now();
    if question.trim().is_empty() {
        emit_route_selection(
            RouteOperation::Clarify,
            "routed",
            "classifier",
            selection_started,
        );
        return Ok(RouteSelection::predetermined(AnswerRoute::EmptyQuestion));
    }
    if planner::is_definitely_unsupported_structured_request(question) {
        debug!(%question, "Skipping query planner for a definitely unsupported structured request");
        emit_route_selection(
            RouteOperation::Unsupported,
            "routed",
            "classifier",
            selection_started,
        );
        return Ok(RouteSelection::predetermined(AnswerRoute::Retrieval(
            RetrievalMode::UnsupportedStructuredQuery,
        )));
    }
    let classifier_started = Instant::now();
    let (response, operation) = match llm.classify_route(question).await {
        Ok(response) => {
            debug!(%question, classifier_response = %response, "Route classifier response received");
            match classifier::parse(&response) {
                Ok(operation) => (response, operation),
                Err(error) => {
                    debug!(%question, classifier_response = %response, %error, "Route classification response rejected");
                    tracing::warn!(%error, "Route classification failed; using non-exhaustive retrieval");
                    emit_route_selection(
                        RouteOperation::Search,
                        "classifier_failure",
                        "classifier",
                        classifier_started,
                    );
                    return Ok(RouteSelection::classifier_failure(
                        AnswerRoute::Retrieval(RetrievalMode::ClassificationFailure),
                        Some(response),
                        format!("{error:#}"),
                    ));
                }
            }
        }
        Err(error) => {
            debug!(%question, %error, "Route classification request failed");
            tracing::warn!(%error, "Route classification failed; using non-exhaustive retrieval");
            emit_route_selection(
                RouteOperation::Search,
                "classifier_failure",
                "classifier",
                classifier_started,
            );
            return Ok(RouteSelection::classifier_failure(
                AnswerRoute::Retrieval(RetrievalMode::ClassificationFailure),
                None,
                format!("{error:#}"),
            ));
        }
    };
    debug!(%question, ?operation, "Route classification accepted");
    match operation {
        RouteOperation::Search => {
            emit_route_selection(operation, "routed", "classifier", classifier_started);
            return Ok(RouteSelection::classified(
                AnswerRoute::Retrieval(RetrievalMode::Ordinary),
                response,
                operation,
            ));
        }
        RouteOperation::Synthesis => {
            emit_route_selection(operation, "routed", "classifier", classifier_started);
            return Ok(RouteSelection::classified(
                AnswerRoute::Synthesis,
                response,
                operation,
            ));
        }
        RouteOperation::Clarify => {
            emit_route_selection(operation, "routed", "classifier", classifier_started);
            return Ok(RouteSelection::classified(
                AnswerRoute::Clarification,
                response,
                operation,
            ));
        }
        RouteOperation::Unsupported => {
            emit_route_selection(operation, "routed", "classifier", classifier_started);
            return Ok(RouteSelection::classified(
                AnswerRoute::Retrieval(RetrievalMode::UnsupportedStructuredQuery),
                response,
                operation,
            ));
        }
        RouteOperation::Count | RouteOperation::List | RouteOperation::CountMembers => {}
    }

    let generator_started = Instant::now();
    let plan = match llm.generate_structured_plan(question, operation).await {
        Ok(response) => match parse_structured_plan(question, &response, operation) {
            Ok(plan) => {
                emit_route_selection(
                    operation,
                    "routed",
                    "structured_generator",
                    generator_started,
                );
                Some(plan)
            }
            Err(error) => {
                debug!(%question, %error, planner_response = %response, ?operation, "Chronicle structured query response rejected");
                emit_route_selection(
                    operation,
                    "validation_failure",
                    "structured_generator",
                    generator_started,
                );
                debug!(
                    %question,
                    ?operation,
                    "Retrying Chronicle structured query with correction request"
                );
                let repair_started = Instant::now();
                match llm
                    .repair_structured_plan(question, operation, &response, &error.to_string())
                    .await
                {
                    Ok(retry_response) => {
                        match parse_structured_plan(question, &retry_response, operation) {
                            Ok(plan) => {
                                debug!(%question, ?plan, "Chronicle structured query retry accepted");
                                emit_route_selection(
                                    operation,
                                    "repaired",
                                    "structured_repair",
                                    repair_started,
                                );
                                Some(plan)
                            }
                            Err(retry_error) => {
                                debug!(%question, %retry_error, planner_response = %retry_response, "Chronicle structured query retry response rejected");
                                tracing::warn!(%retry_error, "Structured query planning failed after retry; using non-exhaustive retrieval");
                                emit_route_selection(
                                    operation,
                                    "repair_failure",
                                    "structured_repair",
                                    repair_started,
                                );
                                None
                            }
                        }
                    }
                    Err(retry_error) => {
                        debug!(%question, %retry_error, initial_error = %error, "Structured query planning retry request failed");
                        tracing::warn!(%retry_error, initial_error = %error, "Structured query planning retry failed; using non-exhaustive retrieval");
                        emit_route_selection(
                            operation,
                            "repair_failure",
                            "structured_repair",
                            repair_started,
                        );
                        None
                    }
                }
            }
        },
        Err(error) => {
            debug!(%question, %error, ?operation, "Structured query generation request failed");
            tracing::warn!(%error, ?operation, "Structured query planning failed; using non-exhaustive retrieval");
            emit_route_selection(
                operation,
                "generation_failure",
                "structured_generator",
                generator_started,
            );
            None
        }
    };
    let Some(mut plan) = plan else {
        return Ok(RouteSelection::classified(
            AnswerRoute::Retrieval(RetrievalMode::PlanningFailure),
            response,
            operation,
        ));
    };
    if plan.selection().is_some() {
        structured_store
            .resolve_string_or_wikilinks(&mut plan)
            .await?;
    }
    debug!(route = ?plan, "Validated Chronicle query plan");
    Ok(RouteSelection::classified(
        AnswerRoute::Structured(plan),
        response,
        operation,
    ))
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
        duration_ms = started.elapsed().as_millis() as u64,
        structured = operation.is_structured(),
        "Chronicle route selection"
    );
}

fn parse_structured_plan(
    question: &str,
    response: &str,
    operation: RouteOperation,
) -> Result<Plan> {
    let plan = planner::parse_for_question(question, response)?;
    ensure!(
        plan.structured_operation() == Some(operation),
        "Structured plan operation did not match classified route"
    );
    Ok(plan)
}
