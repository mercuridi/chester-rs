use anyhow::{Context, Result};
use tracing::debug;

use super::super::{
    indexer::db::repository::facade::IndexerDb,
    llm::LanguageModel,
    query::{plan::Plan, planner},
};

#[derive(Clone, Copy)]
pub(in crate::chronicle::service) enum RetrievalMode {
    Ordinary,
    UnsupportedStructuredQuery,
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

impl RetrievalMode {
    pub(in crate::chronicle::service) fn prefix(self) -> &'static str {
        match self {
            Self::Ordinary => "",
            Self::UnsupportedStructuredQuery => {
                "An exhaustive count or list is unavailable for this question. "
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
            Self::PlanningFailure => format!(
                "{question}\n\nThe structured query planner did not produce a valid plan. Describe only documented examples from the retrieved passages. Do not infer an exhaustive total or claim this is a complete list."
            ),
        }
    }
}

pub(in crate::chronicle::service) async fn select_answer_route(
    llm: &dyn LanguageModel,
    db: Option<&IndexerDb>,
    question: &str,
) -> Result<AnswerRoute> {
    if question.trim().is_empty() {
        return Ok(AnswerRoute::EmptyQuestion);
    }
    if planner::is_definitely_unsupported_structured_request(question) {
        debug!("Skipping query planner for a definitely unsupported structured request");
        return Ok(AnswerRoute::Retrieval(
            RetrievalMode::UnsupportedStructuredQuery,
        ));
    }
    let plan = match llm.generate_plan(question).await {
        Ok(response) => match planner::parse_for_question(question, &response) {
            Ok(plan) => Some(plan),
            Err(error) => {
                debug!(%error, planner_response = %response, "Chronicle query planner response rejected");
                debug!("Retrying Chronicle query planner with correction request");
                match llm
                    .repair_plan(question, &response, &error.to_string())
                    .await
                {
                    Ok(retry_response) => {
                        match planner::parse_for_question(question, &retry_response) {
                            Ok(plan) => {
                                debug!(?plan, "Chronicle query planner retry accepted");
                                Some(plan)
                            }
                            Err(retry_error) => {
                                debug!(%retry_error, planner_response = %retry_response, "Chronicle query planner retry response rejected");
                                tracing::warn!(%retry_error, "Query planning failed after retry; using non-exhaustive retrieval");
                                None
                            }
                        }
                    }
                    Err(retry_error) => {
                        tracing::warn!(%retry_error, initial_error = %error, "Query planning retry failed; using non-exhaustive retrieval");
                        None
                    }
                }
            }
        },
        Err(error) => {
            tracing::warn!(%error, "Query planning failed; using non-exhaustive retrieval");
            None
        }
    };
    let Some(mut plan) = plan else {
        return Ok(AnswerRoute::Retrieval(RetrievalMode::PlanningFailure));
    };
    if plan.selection().is_some() {
        db.context("Structured datastore unavailable")?
            .resolve_string_or_wikilinks(&mut plan)
            .await?;
    }
    debug!(route = ?plan, "Validated Chronicle query plan");
    Ok(match plan {
        Plan::Count { .. } | Plan::List { .. } | Plan::CountMembers { .. } => {
            AnswerRoute::Structured(plan)
        }
        Plan::Clarify {} => AnswerRoute::Clarification,
        Plan::Search {} => AnswerRoute::Retrieval(RetrievalMode::Ordinary),
        Plan::Synthesis {} => AnswerRoute::Synthesis,
        Plan::Unsupported {} => AnswerRoute::Retrieval(RetrievalMode::UnsupportedStructuredQuery),
    })
}
