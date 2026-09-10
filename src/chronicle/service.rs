use super::query::{plan::Plan, planner, render};
use anyhow::{Context, Result};
use std::{collections::BTreeMap, sync::Arc};
use tracing::{debug, info, instrument};

use super::{
    config::SynthesisSettings,
    indexer::{
        db::repository::{AccessScope, IndexerDb, SearchResult},
        prompt,
        retriever::{
            CandidatePoolPolicy, FusionPolicy, RetrievalLimits, RetrievalOutcome, Retriever,
            RetrieverApi, SearchSettings, SelectionPolicy,
        },
    },
    llm::{LanguageModel, Llm},
    runtime::GpuRuntime,
    synthesis::{self, EvidenceNote},
    transcription::service::TranscriptionService,
};

pub struct Chronicle {
    retriever: Arc<dyn RetrieverApi>,
    db: Option<IndexerDb>,
    llm: Arc<dyn LanguageModel>,
    runtime: GpuRuntime,
    transcription: TranscriptionService,
    retrieval_limit: usize,
    retrieval_candidate_limit: usize,
    retrieval_distance_threshold: f32,
    retrieval_near_duplicate_threshold: f32,
    retrieval_max_chunks_per_document: usize,
    pagerank_weight: f64,
    synthesis: SynthesisSettings,
    max_reply_length: usize,
    lifecycle: tokio::sync::Mutex<()>,
    last_synthesis_diagnostics: std::sync::Mutex<Option<SynthesisDiagnostics>>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RetrievedDocumentDiagnostic {
    pub id: String,
    pub rank: usize,
    pub distance: f32,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SynthesisDiagnostics {
    pub retrieved_documents: Vec<RetrievedDocumentDiagnostic>,
    pub accepted_result_count: usize,
    pub omitted_result_count: usize,
    pub chunks_per_document: BTreeMap<String, usize>,
    pub map_batch_count: usize,
    pub reduction_pass_count: usize,
    pub prompt_token_counts: Vec<usize>,
    pub final_answer_length_retried: bool,
    pub final_answer_truncated: bool,
}

enum SynthesisRetrieval {
    Evidence(Vec<SearchResult>),
    ImmediateResponse(&'static str),
}

struct SynthesisEvidenceNotes {
    notes: Vec<EvidenceNote>,
    partial: bool,
    reduction_depth: usize,
}

const PARTIAL_SYNTHESIS_PREFIX: &str =
    "This is a partial synthesis based on the retrieved notes completed so far.\n\n";

impl SynthesisDiagnostics {
    fn from_results(results: &[SearchResult]) -> Self {
        let mut chunks_per_document = BTreeMap::new();
        for result in results {
            *chunks_per_document
                .entry(result.document_path.clone())
                .or_insert(0) += 1;
        }
        Self {
            retrieved_documents: results
                .iter()
                .enumerate()
                .map(|(rank, result)| RetrievedDocumentDiagnostic {
                    id: std::path::Path::new(&result.document_path)
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                    rank: rank + 1,
                    distance: result.distance,
                })
                .collect(),
            accepted_result_count: results.len(),
            omitted_result_count: 0,
            chunks_per_document,
            map_batch_count: 0,
            reduction_pass_count: 0,
            prompt_token_counts: Vec::new(),
            final_answer_length_retried: false,
            final_answer_truncated: false,
        }
    }
}

#[derive(Clone, Copy)]
enum RetrievalMode {
    Ordinary,
    UnsupportedStructuredQuery,
    PlanningFailure,
}

/// A validated answer path. Route selection is complete before route execution begins, keeping
/// structured-query, retrieval, and synthesis policies from leaking into one another.
enum AnswerRoute {
    Structured(Plan),
    Retrieval(RetrievalMode),
    Synthesis,
    Clarification,
    EmptyQuestion,
}

/// The common boundary returned by every answer route. More route-neutral response metadata can
/// be added here without changing `ask_for` or the individual route contracts.
struct AnswerOutcome {
    reply: String,
}

impl AnswerOutcome {
    fn new(reply: String) -> Self {
        Self { reply }
    }

    fn bounded(reply: &str, max_reply_length: usize) -> Self {
        Self::new(reply.chars().take(max_reply_length).collect())
    }
}

impl RetrievalMode {
    fn prefix(self) -> &'static str {
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

    fn retrieval_question(self, question: &str) -> String {
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

impl Chronicle {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: IndexerDb,
        llm: Llm,
        runtime: GpuRuntime,
        retrieval_limit: usize,
        retrieval_candidate_limit: usize,
        retrieval_distance_threshold: f32,
        retrieval_near_duplicate_threshold: f32,
        retrieval_max_chunks_per_document: usize,
        pagerank_weight: f64,
        synthesis: SynthesisSettings,
        max_reply_length: usize,
    ) -> Self {
        Self {
            retriever: Arc::new(Retriever::new(db.clone())),
            db: Some(db),
            llm: Arc::new(llm),
            runtime: runtime.clone(),
            transcription: TranscriptionService::new(runtime),
            retrieval_limit,
            retrieval_candidate_limit,
            retrieval_distance_threshold,
            retrieval_near_duplicate_threshold,
            retrieval_max_chunks_per_document,
            pagerank_weight,
            synthesis,
            max_reply_length,
            lifecycle: tokio::sync::Mutex::new(()),
            last_synthesis_diagnostics: std::sync::Mutex::new(None),
        }
    }

    #[allow(clippy::too_many_arguments)]
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "test dependency injection seam")
    )]
    pub fn with_dependencies(
        retriever: Arc<dyn RetrieverApi>,
        llm: Arc<dyn LanguageModel>,
        runtime: GpuRuntime,
        retrieval_limit: usize,
        retrieval_candidate_limit: usize,
        retrieval_distance_threshold: f32,
        retrieval_near_duplicate_threshold: f32,
        retrieval_max_chunks_per_document: usize,
        pagerank_weight: f64,
        max_reply_length: usize,
    ) -> Self {
        Self {
            retriever,
            db: None,
            llm,
            runtime: runtime.clone(),
            transcription: TranscriptionService::new(runtime),
            retrieval_limit,
            retrieval_candidate_limit,
            retrieval_distance_threshold,
            retrieval_near_duplicate_threshold,
            retrieval_max_chunks_per_document,
            pagerank_weight,
            synthesis: SynthesisSettings::default(),
            max_reply_length,
            lifecycle: tokio::sync::Mutex::new(()),
            last_synthesis_diagnostics: std::sync::Mutex::new(None),
        }
    }

    pub fn last_synthesis_diagnostics(&self) -> Result<Option<SynthesisDiagnostics>> {
        self.last_synthesis_diagnostics
            .lock()
            .map(|diagnostics| diagnostics.clone())
            .map_err(|_| anyhow::anyhow!("synthesis diagnostics state is poisoned"))
    }

    #[instrument(skip(self, question), fields(question_len = question.len()))]
    pub async fn ask(&self, question: &str) -> Result<String> {
        self.ask_for(question, AccessScope::Player).await
    }

    pub async fn ask_for(&self, question: &str, access: AccessScope) -> Result<String> {
        info!("Starting Chronicle question");
        let _lifecycle = self.lifecycle.lock().await;
        let _gpu_lease = self.runtime.acquire_inference()?;

        let route = self.select_answer_route(question).await?;
        let outcome = self.execute_answer_route(question, access, route).await?;
        debug!(reply = %outcome.reply, "Chronicle final reply");
        info!(
            reply_len = outcome.reply.chars().count(),
            "Completed Chronicle question"
        );
        Ok(outcome.reply)
    }

    async fn select_answer_route(&self, question: &str) -> Result<AnswerRoute> {
        if question.trim().is_empty() {
            return Ok(AnswerRoute::EmptyQuestion);
        }
        let plan = match self.llm.generate_plan(question).await {
            Ok(response) => match planner::parse_for_question(question, &response) {
                Ok(plan) => Some(plan),
                Err(error) => {
                    debug!(%error, planner_response = %response, "Chronicle query planner response rejected");
                    debug!("Retrying Chronicle query planner with correction request");
                    match self
                        .llm
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
            self.db
                .as_ref()
                .context("Structured datastore unavailable")?
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
            Plan::Unsupported {} => {
                AnswerRoute::Retrieval(RetrievalMode::UnsupportedStructuredQuery)
            }
        })
    }

    async fn execute_answer_route(
        &self,
        question: &str,
        access: AccessScope,
        route: AnswerRoute,
    ) -> Result<AnswerOutcome> {
        match route {
            AnswerRoute::Structured(plan) => self.answer_from_structured_plan(&plan, access).await,
            AnswerRoute::Retrieval(mode) => {
                self.answer_from_retrieval(question, mode, access).await
            }
            AnswerRoute::Synthesis => self.answer_from_synthesis(question, access).await,
            AnswerRoute::Clarification => Ok(AnswerOutcome::bounded(
                "Please name what you want counted or listed, and any character role or status filters.",
                self.max_reply_length,
            )),
            AnswerRoute::EmptyQuestion => Ok(AnswerOutcome::new(
                "Please provide a non-empty question.".into(),
            )),
        }
    }

    async fn answer_from_structured_plan(
        &self,
        plan: &Plan,
        access: AccessScope,
    ) -> Result<AnswerOutcome> {
        let result = self
            .db
            .as_ref()
            .context("Structured datastore unavailable")?
            .execute_plan_for(plan, access)
            .await?;
        Ok(AnswerOutcome::new(render::render(
            plan,
            &result,
            self.max_reply_length,
        )))
    }

    async fn answer_from_retrieval(
        &self,
        question: &str,
        mode: RetrievalMode,
        access: AccessScope,
    ) -> Result<AnswerOutcome> {
        let prefix = mode.prefix();
        let answer_limit = self.max_reply_length.saturating_sub(prefix.chars().count());
        if answer_limit == 0 {
            return Ok(AnswerOutcome::bounded(prefix, self.max_reply_length));
        }
        let outcome = match self
            .retriever
            .search(
                question,
                SearchSettings {
                    limits: RetrievalLimits {
                        limit: self.retrieval_limit,
                        candidate_limit: self.retrieval_candidate_limit,
                    },
                    candidate_pool: CandidatePoolPolicy {
                        distance_threshold: self.retrieval_distance_threshold,
                    },
                    fusion: FusionPolicy {
                        vector_rrf_weight: 1.0,
                        lexical_rrf_weight: 1.0,
                        pagerank_weight: self.pagerank_weight,
                        rrf_rank_constant: 60.0,
                    },
                    selection: SelectionPolicy {
                        near_duplicate_threshold: self.retrieval_near_duplicate_threshold,
                        max_chunks_per_document: self.retrieval_max_chunks_per_document,
                    },
                },
                access,
            )
            .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                tracing::warn!(%error, "Chronicle retrieval failed");
                return Ok(AnswerOutcome::new(truncate_to_char_limit(
                    &format!("{prefix}Chronicle retrieval failed."),
                    self.max_reply_length,
                )));
            }
        };

        let results = match outcome {
            RetrievalOutcome::Results(results) => results,
            RetrievalOutcome::BadQuestion => {
                return Ok(AnswerOutcome::new(truncate_to_char_limit(
                    &format!("{prefix}Please provide a non-empty question."),
                    self.max_reply_length,
                )));
            }
            RetrievalOutcome::CorpusEmpty => {
                return Ok(AnswerOutcome::new(truncate_to_char_limit(
                    &format!("{prefix}Chronicle corpus is empty."),
                    self.max_reply_length,
                )));
            }
            RetrievalOutcome::NoResultMeetsThreshold => {
                return Ok(AnswerOutcome::new(truncate_to_char_limit(
                    &format!("{prefix}No relevant Chronicle context was found."),
                    self.max_reply_length,
                )));
            }
        };

        let retrieval_question = mode.retrieval_question(question);
        let assembly = prompt::build_prompt_with_budget(
            &retrieval_question,
            &results,
            self.llm.prompt_token_budget(),
            |candidate| self.llm.count_input_tokens(candidate),
        )?;
        let prompt = assembly.prompt;
        debug!(
            result_count = results.len(),
            selected_result_count = assembly.selected_results,
            omitted_result_count = assembly.omitted_results,
            prompt_tokens = assembly.prompt_tokens,
            truncated_result = assembly.truncated_result,
            prompt_len = prompt.len(),
            "Built Chronicle prompt"
        );

        let (answer, _, _) = self.generate_answer(&prompt, answer_limit).await?;
        Ok(AnswerOutcome::new(format!("{prefix}{answer}")))
    }

    async fn answer_from_synthesis(
        &self,
        question: &str,
        access: AccessScope,
    ) -> Result<AnswerOutcome> {
        let results = match self.retrieve_synthesis_evidence(question, access).await {
            SynthesisRetrieval::Evidence(results) => results,
            SynthesisRetrieval::ImmediateResponse(response) => {
                return Ok(AnswerOutcome::new(response.into()));
            }
        };
        let mut diagnostics = SynthesisDiagnostics::from_results(&results);
        let evidence = self
            .generate_evidence_notes(question, &results, &mut diagnostics)
            .await?;
        if evidence.notes.is_empty() {
            return Ok(AnswerOutcome::new(self.incomplete_synthesis_response()));
        }
        let Some(evidence) = self
            .reduce_evidence_notes_until_fit(question, evidence, &mut diagnostics)
            .await?
        else {
            return Ok(AnswerOutcome::new(self.incomplete_synthesis_response()));
        };
        let answer = self
            .generate_synthesis_answer(question, evidence, &mut diagnostics)
            .await?;
        self.record_synthesis_diagnostics(diagnostics)?;
        Ok(AnswerOutcome::new(answer))
    }

    async fn retrieve_synthesis_evidence(
        &self,
        question: &str,
        access: AccessScope,
    ) -> SynthesisRetrieval {
        let settings = SearchSettings {
            limits: RetrievalLimits {
                limit: self.synthesis.retrieval_limit,
                candidate_limit: self.synthesis.candidate_limit,
            },
            candidate_pool: CandidatePoolPolicy {
                distance_threshold: self.retrieval_distance_threshold,
            },
            fusion: FusionPolicy {
                vector_rrf_weight: 1.0,
                lexical_rrf_weight: 1.0,
                pagerank_weight: self.pagerank_weight,
                rrf_rank_constant: 60.0,
            },
            selection: SelectionPolicy {
                near_duplicate_threshold: self.retrieval_near_duplicate_threshold,
                max_chunks_per_document: self.synthesis.max_chunks_per_document,
            },
        };
        match self.retriever.search(question, settings, access).await {
            Ok(RetrievalOutcome::Results(results)) => SynthesisRetrieval::Evidence(results),
            Ok(RetrievalOutcome::BadQuestion) => {
                SynthesisRetrieval::ImmediateResponse("Please provide a non-empty question.")
            }
            Ok(RetrievalOutcome::CorpusEmpty) => {
                SynthesisRetrieval::ImmediateResponse("Chronicle corpus is empty.")
            }
            Ok(RetrievalOutcome::NoResultMeetsThreshold) => {
                SynthesisRetrieval::ImmediateResponse("No relevant Chronicle context was found.")
            }
            Err(error) => {
                tracing::warn!(%error, "Chronicle synthesis retrieval failed");
                SynthesisRetrieval::ImmediateResponse("Chronicle retrieval failed.")
            }
        }
    }

    async fn generate_evidence_notes(
        &self,
        question: &str,
        results: &[SearchResult],
        diagnostics: &mut SynthesisDiagnostics,
    ) -> Result<SynthesisEvidenceNotes> {
        let token_budget = self
            .synthesis
            .batch_token_budget
            .min(self.llm.prompt_token_budget());
        let sources = synthesis::retrieved_evidence(results);
        let map_plan = synthesis::pack_batches(
            question,
            &sources,
            token_budget,
            self.synthesis.max_batches,
            synthesis::map_prompt,
            |prompt| self.llm.count_input_tokens(prompt),
        )?;
        debug!(
            retrieved_result_count = results.len(),
            selected_result_count = results.len().saturating_sub(map_plan.omitted_items),
            map_batch_count = map_plan.batches.len(),
            omitted_result_count = map_plan.omitted_items,
            coverage_constrained_by_max_batches = map_plan.omitted_items > 0,
            batch_token_budget = token_budget,
            "Built bounded Chronicle synthesis map plan"
        );
        diagnostics.omitted_result_count = map_plan.omitted_items;
        diagnostics.map_batch_count = map_plan.batches.len();

        let mut notes = Vec::with_capacity(map_plan.batches.len());
        let mut partial = false;
        for batch in map_plan.batches {
            let source_labels = synthesis::merged_labels(&batch);
            let prompt = synthesis::map_prompt(question, &batch);
            let prompt_tokens = self.llm.count_input_tokens(&prompt)?;
            diagnostics.prompt_token_counts.push(prompt_tokens);
            debug!(
                source_labels = ?source_labels,
                source_count = batch.len(),
                prompt_tokens,
                "Generating Chronicle synthesis evidence note"
            );
            match self.llm.generate(&prompt).await {
                Ok(text) => notes.push(EvidenceNote {
                    source_labels,
                    text,
                }),
                Err(error) => {
                    tracing::warn!(%error, completed_note_count = notes.len(), "Chronicle synthesis map generation failed");
                    partial = true;
                    break;
                }
            }
        }
        debug!(
            intermediate_note_count = notes.len(),
            partial, "Completed Chronicle synthesis map stage"
        );
        Ok(SynthesisEvidenceNotes {
            notes,
            partial,
            reduction_depth: 0,
        })
    }

    async fn reduce_evidence_notes_until_fit(
        &self,
        question: &str,
        mut evidence: SynthesisEvidenceNotes,
        diagnostics: &mut SynthesisDiagnostics,
    ) -> Result<Option<SynthesisEvidenceNotes>> {
        let token_budget = self
            .synthesis
            .batch_token_budget
            .min(self.llm.prompt_token_budget());
        while !synthesis::final_prompt_fits(
            question,
            &evidence.notes,
            evidence.partial,
            self.llm.prompt_token_budget(),
            |prompt| self.llm.count_input_tokens(prompt),
        )? {
            let reduction_plan = synthesis::pack_batches(
                question,
                &evidence.notes,
                token_budget,
                evidence.notes.len(),
                synthesis::reduce_prompt,
                |prompt| self.llm.count_input_tokens(prompt),
            )?;
            diagnostics.reduction_pass_count += 1;
            if reduction_plan.batches.len() >= evidence.notes.len() {
                tracing::warn!(
                    evidence_note_count = evidence.notes.len(),
                    "Chronicle synthesis reduction could not make progress"
                );
                return Ok(None);
            }
            let mut reduced = Vec::with_capacity(reduction_plan.batches.len());
            for batch in reduction_plan.batches {
                let source_labels = synthesis::merged_labels(&batch);
                let prompt = synthesis::reduce_prompt(question, &batch);
                let prompt_tokens = self.llm.count_input_tokens(&prompt)?;
                diagnostics.prompt_token_counts.push(prompt_tokens);
                debug!(
                    reduction_depth = evidence.reduction_depth,
                    source_labels = ?source_labels,
                    source_note_count = batch.len(),
                    prompt_tokens,
                    "Generating Chronicle synthesis reduction note"
                );
                match self.llm.generate(&prompt).await {
                    Ok(text) => reduced.push(EvidenceNote {
                        source_labels,
                        text,
                    }),
                    Err(error) => {
                        tracing::warn!(%error, reduction_depth = evidence.reduction_depth, completed_note_count = reduced.len(), "Chronicle synthesis reduction generation failed");
                        evidence.partial = true;
                        break;
                    }
                }
            }
            if reduced.is_empty() {
                return Ok(None);
            }
            evidence.notes = reduced;
            evidence.reduction_depth += 1;
            debug!(
                reduction_depth = evidence.reduction_depth,
                intermediate_note_count = evidence.notes.len(),
                partial = evidence.partial,
                "Completed Chronicle synthesis reduction pass"
            );
        }
        Ok(Some(evidence))
    }

    async fn generate_synthesis_answer(
        &self,
        question: &str,
        evidence: SynthesisEvidenceNotes,
        diagnostics: &mut SynthesisDiagnostics,
    ) -> Result<String> {
        let coverage_ledger = synthesis::CoverageLedger::new(evidence.notes, evidence.partial);
        debug!(
            reduction_depth = evidence.reduction_depth,
            partial = evidence.partial,
            coverage_ledger = %coverage_ledger.debug_artifact(),
            "Completed Chronicle synthesis coverage ledger"
        );
        let final_prompt = synthesis::final_prompt_with_partial_status(
            question,
            coverage_ledger.notes(),
            evidence.partial,
        );
        let final_prompt_tokens = self.llm.count_input_tokens(&final_prompt)?;
        diagnostics.prompt_token_counts.push(final_prompt_tokens);
        debug!(
            reduction_depth = evidence.reduction_depth,
            evidence_note_count = coverage_ledger.notes().len(),
            final_prompt_tokens,
            prompt_token_budget = self.llm.prompt_token_budget(),
            partial = evidence.partial,
            "Built Chronicle synthesis answer prompt"
        );
        let answer_limit = if evidence.partial {
            self.max_reply_length
                .saturating_sub(PARTIAL_SYNTHESIS_PREFIX.chars().count())
        } else {
            self.max_reply_length
        };
        let (answer, retried, truncated) =
            self.generate_answer(&final_prompt, answer_limit).await?;
        diagnostics.final_answer_length_retried = retried;
        diagnostics.final_answer_truncated = truncated;
        Ok(if evidence.partial {
            format!("{PARTIAL_SYNTHESIS_PREFIX}{answer}")
        } else {
            answer
        })
    }

    fn record_synthesis_diagnostics(&self, diagnostics: SynthesisDiagnostics) -> Result<()> {
        self.last_synthesis_diagnostics
            .lock()
            .map_err(|_| anyhow::anyhow!("synthesis diagnostics state is poisoned"))?
            .replace(diagnostics);
        Ok(())
    }

    fn incomplete_synthesis_response(&self) -> String {
        truncate_to_char_limit(
            "Chronicle synthesis could not be completed from the retrieved notes.",
            self.max_reply_length,
        )
    }

    async fn generate_answer(
        &self,
        prompt: &str,
        answer_limit: usize,
    ) -> Result<(String, bool, bool)> {
        let mut answer = self.llm.generate(prompt).await?;
        let mut retried = false;
        let mut truncated = false;

        if answer.chars().count() > answer_limit {
            debug!(
                answer_len = answer.chars().count(),
                max_reply_length = answer_limit,
                "LLM answer exceeded configured length; requesting a shorter answer"
            );
            let retry_prompt = format!(
                "{prompt}\n\nThe draft answer below is too long. Rewrite it to fit within {answer_limit} characters. Preserve the most important information, and output only the shorter answer.\n\nDraft answer:\n{answer}"
            );
            answer = self.llm.generate(&retry_prompt).await?;
            retried = true;
        }

        if answer.chars().count() > answer_limit {
            tracing::warn!(
                answer_len = answer.chars().count(),
                max_reply_length = answer_limit,
                "LLM answer remained over length after retry; truncating"
            );
            answer = truncate_to_char_limit(&answer, answer_limit);
            truncated = true;
        }
        Ok((answer, retried, truncated))
    }

    #[instrument(skip(self))]
    pub async fn start_llm(&self) -> Result<()> {
        let _lifecycle = self.lifecycle.lock().await;
        info!("Starting Chronicle models");

        self.retriever.load_embedder().await?;

        if let Err(error) = self.llm.load().await {
            tracing::warn!(%error, "Chronicle LLM failed to load; releasing embedder");
            self.retriever.unload_embedder()?;
            return Err(error);
        }

        info!("Chronicle models ready");
        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn stop_llm(&self) -> Result<()> {
        let _lifecycle = self.lifecycle.lock().await;
        info!("Stopping Chronicle models");

        self.llm.unload().await?;
        self.retriever.unload_embedder()?;
        info!("Chronicle models stopped");
        Ok(())
    }

    pub fn is_llm_loaded(&self) -> Result<bool> {
        self.runtime.is_llm_loaded()
    }

    pub fn transcription_service(&self) -> TranscriptionService {
        self.transcription.clone()
    }
}

fn truncate_to_char_limit(answer: &str, max_length: usize) -> String {
    answer.chars().take(max_length).collect()
}

#[cfg(test)]
#[allow(clippy::type_complexity, clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::{Chronicle, truncate_to_char_limit};
    use crate::chronicle::indexer::db::repository::IndexerDb;
    use crate::chronicle::{
        indexer::{
            db::repository::SearchResult,
            retriever::{RetrievalOutcome, RetrieverApi, SearchSettings},
        },
        llm::LanguageModel,
        runtime::GpuRuntime,
    };
    use anyhow::Context;
    use anyhow::{Result, anyhow};
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };

    #[derive(Clone, Copy)]
    enum FakeOutcome {
        Results,
        TwoResults,
        FourResults,
        EightResults,
        BadQuestion,
        CorpusEmpty,
        NoResult,
        Error,
    }

    struct FakeRetriever {
        outcome: FakeOutcome,
        calls: Mutex<Vec<(String, usize, usize, f32, f32, usize)>>,
        accesses: Mutex<Vec<crate::chronicle::indexer::db::repository::AccessScope>>,
        loads: Mutex<usize>,
        unloads: Mutex<usize>,
    }

    impl FakeRetriever {
        fn new(outcome: FakeOutcome) -> Self {
            Self {
                outcome,
                calls: Mutex::new(Vec::new()),
                accesses: Mutex::new(Vec::new()),
                loads: Mutex::new(0),
                unloads: Mutex::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl RetrieverApi for FakeRetriever {
        async fn search(
            &self,
            query: &str,
            settings: SearchSettings,
            _access: crate::chronicle::indexer::db::repository::AccessScope,
        ) -> Result<RetrievalOutcome> {
            self.calls
                .lock()
                .map_err(|_| anyhow!("calls poisoned"))?
                .push((
                    query.into(),
                    settings.limits.limit,
                    settings.limits.candidate_limit,
                    settings.candidate_pool.distance_threshold,
                    settings.selection.near_duplicate_threshold,
                    settings.selection.max_chunks_per_document,
                ));
            self.accesses
                .lock()
                .map_err(|_| anyhow!("accesses poisoned"))?
                .push(_access);
            match self.outcome {
                FakeOutcome::Results => Ok(RetrievalOutcome::Results(vec![SearchResult {
                    document_path: "doc.md".into(),
                    chunk_index: 0,
                    heading: None,
                    text: "context".into(),
                    overlaps_previous: false,
                    distance: 0.1,
                }])),
                FakeOutcome::TwoResults => Ok(RetrievalOutcome::Results(vec![
                    SearchResult {
                        document_path: "one.md".into(),
                        chunk_index: 0,
                        heading: None,
                        text: "first context".into(),
                        overlaps_previous: false,
                        distance: 0.1,
                    },
                    SearchResult {
                        document_path: "two.md".into(),
                        chunk_index: 0,
                        heading: None,
                        text: "second context".into(),
                        overlaps_previous: false,
                        distance: 0.1,
                    },
                ])),
                FakeOutcome::FourResults => Ok(RetrievalOutcome::Results(
                    ["one", "two", "three", "four"]
                        .into_iter()
                        .map(|text| SearchResult {
                            document_path: format!("{text}.md"),
                            chunk_index: 0,
                            heading: None,
                            text: format!("{text} context"),
                            overlaps_previous: false,
                            distance: 0.1,
                        })
                        .collect(),
                )),
                FakeOutcome::EightResults => Ok(RetrievalOutcome::Results(
                    (0..8)
                        .map(|index| SearchResult {
                            document_path: format!("{index}.md"),
                            chunk_index: 0,
                            heading: None,
                            text: format!("context {index}"),
                            overlaps_previous: false,
                            distance: 0.1,
                        })
                        .collect(),
                )),
                FakeOutcome::BadQuestion => Ok(RetrievalOutcome::BadQuestion),
                FakeOutcome::CorpusEmpty => Ok(RetrievalOutcome::CorpusEmpty),
                FakeOutcome::NoResult => Ok(RetrievalOutcome::NoResultMeetsThreshold),
                FakeOutcome::Error => Err(anyhow!("retrieval failed")),
            }
        }

        async fn load_embedder(&self) -> Result<()> {
            *self.loads.lock().map_err(|_| anyhow!("loads poisoned"))? += 1;
            Ok(())
        }

        fn unload_embedder(&self) -> Result<()> {
            *self
                .unloads
                .lock()
                .map_err(|_| anyhow!("unloads poisoned"))? += 1;
            Ok(())
        }
    }

    struct FakeLlm {
        runtime: GpuRuntime,
        outputs: Mutex<VecDeque<String>>,
        prompts: Mutex<Vec<String>>,
        budget: Mutex<usize>,
        plan_output: Mutex<String>,
        repair_plan_output: Mutex<Option<String>>,
        repair_requests: Mutex<Vec<(String, String, String)>>,
        fail_count: bool,
        fail_generate_on_call: Mutex<Option<usize>>,
        generate_calls: Mutex<usize>,
        fail_load: bool,
        loads: Mutex<usize>,
        unloads: Mutex<usize>,
    }

    impl FakeLlm {
        fn new(runtime: GpuRuntime, outputs: impl IntoIterator<Item = &'static str>) -> Self {
            Self {
                runtime,
                outputs: Mutex::new(outputs.into_iter().map(str::to_owned).collect()),
                prompts: Mutex::new(Vec::new()),
                budget: Mutex::new(10_000),
                plan_output: Mutex::new(r#"{"operation":"search"}"#.into()),
                repair_plan_output: Mutex::new(None),
                repair_requests: Mutex::new(Vec::new()),
                fail_count: false,
                fail_generate_on_call: Mutex::new(None),
                generate_calls: Mutex::new(0),
                fail_load: false,
                loads: Mutex::new(0),
                unloads: Mutex::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl LanguageModel for FakeLlm {
        fn prompt_token_budget(&self) -> usize {
            *self.budget.lock().expect("budget poisoned")
        }

        fn count_input_tokens(&self, prompt: &str) -> Result<usize> {
            if self.fail_count {
                return Err(anyhow!("token counting failed"));
            }
            Ok(prompt.chars().count())
        }

        async fn generate_plan(&self, _question: &str) -> Result<String> {
            Ok(self
                .plan_output
                .lock()
                .map_err(|_| anyhow!("plan poisoned"))?
                .clone())
        }

        async fn repair_plan(
            &self,
            question: &str,
            rejected_response: &str,
            rejection_error: &str,
        ) -> Result<String> {
            self.repair_requests
                .lock()
                .map_err(|_| anyhow!("repair requests poisoned"))?
                .push((
                    question.into(),
                    rejected_response.into(),
                    rejection_error.into(),
                ));
            self.repair_plan_output
                .lock()
                .map_err(|_| anyhow!("repair plan poisoned"))?
                .take()
                .ok_or_else(|| anyhow!("no fake repair output"))
        }

        async fn generate(&self, prompt: &str) -> Result<String> {
            self.prompts
                .lock()
                .map_err(|_| anyhow!("prompts poisoned"))?
                .push(prompt.into());
            let call = {
                let mut calls = self
                    .generate_calls
                    .lock()
                    .map_err(|_| anyhow!("generate counter poisoned"))?;
                *calls += 1;
                *calls
            };
            if self
                .fail_generate_on_call
                .lock()
                .map_err(|_| anyhow!("generate failure setting poisoned"))?
                .is_some_and(|failed_call| failed_call == call)
            {
                return Err(anyhow!("generation failed"));
            }
            self.outputs
                .lock()
                .map_err(|_| anyhow!("outputs poisoned"))?
                .pop_front()
                .ok_or_else(|| anyhow!("no fake output"))
        }

        async fn load(&self) -> Result<()> {
            *self.loads.lock().map_err(|_| anyhow!("loads poisoned"))? += 1;
            let lease = self.runtime.begin_llm_load()?;
            if self.fail_load {
                return Err(anyhow!("load failed"));
            }
            lease.commit_to_loaded()
        }

        async fn unload(&self) -> Result<()> {
            *self
                .unloads
                .lock()
                .map_err(|_| anyhow!("unloads poisoned"))? += 1;
            self.runtime.begin_llm_unload()?.commit_to_idle()
        }
    }

    fn service(
        outcome: FakeOutcome,
        outputs: impl IntoIterator<Item = &'static str>,
        max_reply_length: usize,
    ) -> Result<(Chronicle, Arc<FakeRetriever>, Arc<FakeLlm>)> {
        let runtime = GpuRuntime::new();
        runtime.begin_llm_load()?.commit_to_loaded()?;
        let retriever = Arc::new(FakeRetriever::new(outcome));
        let llm = Arc::new(FakeLlm::new(runtime.clone(), outputs));
        let chronicle = Chronicle::with_dependencies(
            retriever.clone(),
            llm.clone(),
            runtime,
            5,
            15,
            0.8,
            0.85,
            2,
            0.15,
            max_reply_length,
        );
        Ok((chronicle, retriever, llm))
    }

    fn mutex_value(value: &Mutex<usize>) -> Result<usize> {
        value
            .lock()
            .map(|guard| *guard)
            .map_err(|_| anyhow!("counter poisoned"))
    }

    #[tokio::test]
    async fn structured_zero_and_list_bypass_retrieval_and_answer_generation() -> Result<()> {
        let (mut chronicle, retriever, llm) = service(FakeOutcome::Error, [], 500)?;
        let directory = tempfile::tempdir()?;
        let db = IndexerDb::open(&format!(
            "sqlite://{}",
            directory.path().join("test.sqlite3").display()
        ))
        .await?;
        let (metadata, _) = crate::chronicle::indexer::frontmatter::parse("---\nid: ada\ntype: character\nstatus: canon\nvisibility: player\ncreated: 2026-09-07\nupdated: 2026-09-07\nrole: npc\nlife_status: alive\n---\n")?.context("note")?;
        db.replace_note("Ada.md", "hash", &[], &[], &metadata)
            .await?;
        chronicle.db = Some(db);
        *llm.plan_output.lock().map_err(|_| anyhow!("plan poisoned"))? = r#"{"operation":"count","note_type":"character","filters":{"role":"pc","life_status":"dead"}}"#.into();
        let answer = chronicle.ask("How many dead PCs?").await?;
        assert!(answer.starts_with("0 canon PCs recorded"));
        *llm.plan_output
            .lock()
            .map_err(|_| anyhow!("plan poisoned"))? =
            r#"{"operation":"list","note_type":"character","filters":{"role":"npc"}}"#.into();
        let answer = chronicle.ask("List NPCs").await?;
        assert!(answer.contains("Ada [ada]"));
        assert!(
            retriever
                .calls
                .lock()
                .map_err(|_| anyhow!("calls poisoned"))?
                .is_empty()
        );
        assert!(
            llm.prompts
                .lock()
                .map_err(|_| anyhow!("prompts poisoned"))?
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn structured_generic_played_by_list_bypasses_retrieval_and_answer_generation()
    -> Result<()> {
        let (mut chronicle, retriever, llm) = service(FakeOutcome::Error, [], 500)?;
        let directory = tempfile::tempdir()?;
        let db = IndexerDb::open(&format!(
            "sqlite://{}",
            directory.path().join("test.sqlite3").display()
        ))
        .await?;
        for (id, title) in [("garr", "Garr"), ("jora", "Jora")] {
            let (metadata, _) = crate::chronicle::indexer::frontmatter::parse(&format!(
                "---\nid: {id}\ntype: character\nstatus: canon\nvisibility: player\ncreated: 2026-09-07\nupdated: 2026-09-07\nrole: pc\nplayed_by: Rowan\n---\n"
            ))?
            .context("note")?;
            db.replace_note(&format!("{title}.md"), "hash", &[], &[], &metadata)
                .await?;
        }
        chronicle.db = Some(db);
        *llm.plan_output
            .lock()
            .map_err(|_| anyhow!("plan poisoned"))? = "This is not a JSON query plan.".into();
        *llm
            .repair_plan_output
            .lock()
            .map_err(|_| anyhow!("repair plan poisoned"))? = Some(r#"{"operation":"list","note_type":"character","filters":{"role":"pc","conditions":[{"field":"played_by","operator":"equals","value":"Rowan"}]}}"#.into());

        let answer = chronicle.ask("List all PCs played by Rowan.").await?;

        assert!(answer.starts_with("2 canon PCs recorded"));
        assert!(answer.contains("Garr [garr]"));
        assert!(answer.contains("Jora [jora]"));
        let repair_requests = llm
            .repair_requests
            .lock()
            .map_err(|_| anyhow!("repair requests poisoned"))?;
        assert_eq!(repair_requests.len(), 1);
        assert_eq!(repair_requests[0].0, "List all PCs played by Rowan.");
        assert_eq!(repair_requests[0].1, "This is not a JSON query plan.");
        assert!(repair_requests[0].2.contains("Invalid query plan JSON"));
        assert!(
            retriever
                .calls
                .lock()
                .map_err(|_| anyhow!("calls poisoned"))?
                .is_empty()
        );
        assert!(
            llm.prompts
                .lock()
                .map_err(|_| anyhow!("prompts poisoned"))?
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn invalid_plan_uses_best_effort_retrieval_and_clarification_does_not_search()
    -> Result<()> {
        let (chronicle, retriever, llm) =
            service(FakeOutcome::Results, ["Some documented examples."], 500)?;
        *llm.plan_output
            .lock()
            .map_err(|_| anyhow!("plan poisoned"))? =
            r#"{"operation":"count","note_type":"character","filters":{"character_status":"alive"}}"#
                .into();
        let answer = chronicle.ask("How many characters in Northmere?").await?;
        assert!(answer.starts_with("I couldn't validate a structured plan"));
        assert!(
            llm.prompts
                .lock()
                .map_err(|_| anyhow!("prompts poisoned"))?[0]
                .contains("planner did not produce a valid plan")
        );
        *llm.plan_output
            .lock()
            .map_err(|_| anyhow!("plan poisoned"))? = r#"{"operation":"clarify"}"#.into();
        assert!(chronicle.ask("List them").await?.starts_with("Please name"));
        assert_eq!(
            retriever
                .calls
                .lock()
                .map_err(|_| anyhow!("calls poisoned"))?
                .len(),
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn unsupported_plan_uses_non_exhaustive_retrieval() -> Result<()> {
        let (chronicle, _retriever, llm) =
            service(FakeOutcome::Results, ["Some documented examples."], 500)?;
        *llm.plan_output
            .lock()
            .map_err(|_| anyhow!("plan poisoned"))? = r#"{"operation":"unsupported"}"#.into();

        let answer = chronicle.ask("How many enemies does Ada have?").await?;

        assert!(answer.starts_with("An exhaustive count or list is unavailable"));
        assert!(
            llm.prompts
                .lock()
                .map_err(|_| anyhow!("prompts poisoned"))?[0]
                .contains("cannot be executed as a structured count or list")
        );
        Ok(())
    }

    #[test]
    fn truncation_is_unicode_safe_and_handles_zero() {
        assert_eq!(truncate_to_char_limit("éclair", 2), "éc");
        assert_eq!(truncate_to_char_limit("answer", 0), "");
        assert_eq!(truncate_to_char_limit("short", 10), "short");
    }

    #[tokio::test]
    async fn ask_passes_retrieval_configuration_and_generates_answer() -> Result<()> {
        let (chronicle, retriever, llm) = service(FakeOutcome::Results, ["answer"], 100)?;
        assert_eq!(chronicle.ask("question").await?, "answer");
        let calls = retriever
            .calls
            .lock()
            .map_err(|_| anyhow!("calls poisoned"))?;
        assert_eq!(
            calls.as_slice(),
            &[("question".into(), 5, 15, 0.8, 0.85, 2)]
        );
        let prompts = llm
            .prompts
            .lock()
            .map_err(|_| anyhow!("prompts poisoned"))?;
        assert_eq!(prompts.len(), 1);
        assert!(prompts[0].contains("Document: doc"));
        assert!(prompts[0].contains("Question:\nquestion"));
        Ok(())
    }

    #[tokio::test]
    async fn synthesis_plan_uses_bounded_retrieval_and_map_reduce() -> Result<()> {
        let (chronicle, retriever, llm) =
            service(FakeOutcome::Results, ["evidence note", "answer"], 100)?;
        *llm.plan_output
            .lock()
            .map_err(|_| anyhow!("plan poisoned"))? = r#"{"operation":"synthesis"}"#.into();

        assert_eq!(
            chronicle.ask("Summarise the history of Northmere.").await?,
            "answer"
        );
        assert_eq!(
            retriever
                .calls
                .lock()
                .map_err(|_| anyhow!("calls poisoned"))?
                .as_slice(),
            &[(
                "Summarise the history of Northmere.".into(),
                12,
                40,
                0.8,
                0.85,
                3
            )]
        );
        assert_eq!(
            llm.prompts
                .lock()
                .map_err(|_| anyhow!("prompts poisoned"))?
                .len(),
            2
        );
        let prompts = llm
            .prompts
            .lock()
            .map_err(|_| anyhow!("prompts poisoned"))?;
        assert!(prompts[0].contains("<evidence>"));
        assert!(prompts[1].contains("<coverage_ledger>"));
        Ok(())
    }

    #[tokio::test]
    async fn synthesis_preserves_the_callers_access_scope() -> Result<()> {
        let (chronicle, retriever, llm) =
            service(FakeOutcome::Results, ["evidence note", "answer"], 100)?;
        *llm.plan_output
            .lock()
            .map_err(|_| anyhow!("plan poisoned"))? = r#"{"operation":"synthesis"}"#.into();

        assert_eq!(
            chronicle
                .ask_for(
                    "Summarise the history of Northmere.",
                    crate::chronicle::indexer::db::repository::AccessScope::Gm,
                )
                .await?,
            "answer"
        );
        assert_eq!(
            retriever
                .accesses
                .lock()
                .map_err(|_| anyhow!("accesses poisoned"))?
                .as_slice(),
            &[crate::chronicle::indexer::db::repository::AccessScope::Gm]
        );
        Ok(())
    }

    #[tokio::test]
    async fn synthesis_short_circuits_retrieval_outcomes() -> Result<()> {
        let cases = [
            (
                FakeOutcome::BadQuestion,
                "Please provide a non-empty question.",
            ),
            (FakeOutcome::CorpusEmpty, "Chronicle corpus is empty."),
            (
                FakeOutcome::NoResult,
                "No relevant Chronicle context was found.",
            ),
            (FakeOutcome::Error, "Chronicle retrieval failed."),
        ];
        for (outcome, expected) in cases {
            let (chronicle, _retriever, llm) = service(outcome, [], 100)?;
            *llm.plan_output
                .lock()
                .map_err(|_| anyhow!("plan poisoned"))? = r#"{"operation":"synthesis"}"#.into();
            assert_eq!(chronicle.ask("Summarise Northmere.").await?, expected);
            assert!(
                llm.prompts
                    .lock()
                    .map_err(|_| anyhow!("prompts poisoned"))?
                    .is_empty()
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn synthesis_reports_an_honest_failure_when_no_evidence_note_can_be_generated()
    -> Result<()> {
        let (chronicle, _retriever, llm) = service(FakeOutcome::Results, [], 100)?;
        *llm.plan_output
            .lock()
            .map_err(|_| anyhow!("plan poisoned"))? = r#"{"operation":"synthesis"}"#.into();
        *llm.fail_generate_on_call
            .lock()
            .map_err(|_| anyhow!("generate failure setting poisoned"))? = Some(1);

        assert_eq!(
            chronicle.ask("Summarise the history of Northmere.").await?,
            "Chronicle synthesis could not be completed from the retrieved notes."
        );
        Ok(())
    }

    #[tokio::test]
    async fn synthesis_qualifies_a_partial_answer_after_a_map_failure() -> Result<()> {
        let (mut chronicle, _retriever, llm) =
            service(FakeOutcome::TwoResults, ["first evidence", "answer"], 200)?;
        *llm.plan_output
            .lock()
            .map_err(|_| anyhow!("plan poisoned"))? = r#"{"operation":"synthesis"}"#.into();
        let question = "Summarise the history of Northmere.";
        let sources = crate::chronicle::synthesis::retrieved_evidence(&[
            SearchResult {
                document_path: "one.md".into(),
                chunk_index: 0,
                heading: None,
                text: "first context".into(),
                overlaps_previous: false,
                distance: 0.1,
            },
            SearchResult {
                document_path: "two.md".into(),
                chunk_index: 0,
                heading: None,
                text: "second context".into(),
                overlaps_previous: false,
                distance: 0.1,
            },
        ]);
        chronicle.synthesis.batch_token_budget =
            crate::chronicle::synthesis::map_prompt(question, &sources[..1])
                .len()
                .max(crate::chronicle::synthesis::map_prompt(question, &sources[1..]).len());
        *llm.fail_generate_on_call
            .lock()
            .map_err(|_| anyhow!("generate failure setting poisoned"))? = Some(2);

        let answer = chronicle.ask(question).await?;
        assert!(answer.starts_with(
            "This is a partial synthesis based on the retrieved notes completed so far."
        ));
        assert!(answer.ends_with("answer"));
        Ok(())
    }

    #[tokio::test]
    async fn synthesis_reduces_evidence_before_final_answer() -> Result<()> {
        let (mut chronicle, _retriever, llm) = service(
            FakeOutcome::FourResults,
            ["m1", "m2", "m3", "m4", "r1", "answer"],
            500,
        )?;
        *llm.plan_output
            .lock()
            .map_err(|_| anyhow!("plan poisoned"))? = r#"{"operation":"synthesis"}"#.into();
        let question = "Summarise the history of Northmere.";
        let sources = crate::chronicle::synthesis::retrieved_evidence(&[
            SearchResult {
                document_path: "one.md".into(),
                chunk_index: 0,
                heading: None,
                text: "one context".into(),
                overlaps_previous: false,
                distance: 0.1,
            },
            SearchResult {
                document_path: "two.md".into(),
                chunk_index: 0,
                heading: None,
                text: "two context".into(),
                overlaps_previous: false,
                distance: 0.1,
            },
            SearchResult {
                document_path: "three.md".into(),
                chunk_index: 0,
                heading: None,
                text: "three context".into(),
                overlaps_previous: false,
                distance: 0.1,
            },
            SearchResult {
                document_path: "four.md".into(),
                chunk_index: 0,
                heading: None,
                text: "four context".into(),
                overlaps_previous: false,
                distance: 0.1,
            },
        ]);
        let map_budget = (0..4)
            .map(|index| {
                crate::chronicle::synthesis::map_prompt(question, &sources[index..=index]).len()
            })
            .max()
            .unwrap();
        chronicle.synthesis.batch_token_budget = map_budget;
        *llm.budget.lock().map_err(|_| anyhow!("budget poisoned"))? =
            crate::chronicle::synthesis::final_prompt_with_partial_status(
                question,
                &[1, 2, 3, 4]
                    .into_iter()
                    .map(|index| super::super::synthesis::EvidenceNote {
                        source_labels: vec![format!("S{index}")],
                        text: format!("m{index}"),
                    })
                    .collect::<Vec<_>>(),
                false,
            )
            .len()
            .saturating_sub(1);
        let answer = chronicle.ask(question).await?;
        assert_eq!(answer, "answer");
        let prompts = llm
            .prompts
            .lock()
            .map_err(|_| anyhow!("prompts poisoned"))?;
        assert_eq!(
            prompts
                .iter()
                .filter(|prompt| prompt.contains("<evidence>"))
                .count(),
            4
        );
        assert_eq!(
            prompts
                .iter()
                .filter(|prompt| prompt.contains("<evidence_notes>"))
                .count(),
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn synthesis_surfaces_a_late_reduction_pipeline_failure() -> Result<()> {
        let (mut chronicle, _retriever, llm) = service(
            FakeOutcome::EightResults,
            ["m1", "m2", "m3", "m4", "m5", "m6", "answer", "answer"],
            500,
        )?;
        *llm.plan_output
            .lock()
            .map_err(|_| anyhow!("plan poisoned"))? = r#"{"operation":"synthesis"}"#.into();
        let question = "Summarise the history of Northmere.";
        let mut sources = crate::chronicle::synthesis::retrieved_evidence(&[
            SearchResult {
                document_path: "one.md".into(),
                chunk_index: 0,
                heading: None,
                text: "one context".into(),
                overlaps_previous: false,
                distance: 0.1,
            },
            SearchResult {
                document_path: "two.md".into(),
                chunk_index: 0,
                heading: None,
                text: "two context".into(),
                overlaps_previous: false,
                distance: 0.1,
            },
            SearchResult {
                document_path: "three.md".into(),
                chunk_index: 0,
                heading: None,
                text: "three context".into(),
                overlaps_previous: false,
                distance: 0.1,
            },
            SearchResult {
                document_path: "four.md".into(),
                chunk_index: 0,
                heading: None,
                text: "four context".into(),
                overlaps_previous: false,
                distance: 0.1,
            },
        ]);
        sources.extend(sources.clone());
        let map_budget = crate::chronicle::synthesis::map_prompt(question, &sources[..1]).len();
        chronicle.synthesis.batch_token_budget = map_budget + 20;
        *llm.budget.lock().map_err(|_| anyhow!("budget poisoned"))? =
            crate::chronicle::synthesis::final_prompt_with_partial_status(
                question,
                &[1, 2, 3, 4, 5, 6, 7, 8]
                    .into_iter()
                    .map(|index| super::super::synthesis::EvidenceNote {
                        source_labels: vec![format!("S{index}")],
                        text: format!("m{index}"),
                    })
                    .collect::<Vec<_>>(),
                false,
            )
            .len()
            .saturating_sub(1);
        *llm.fail_generate_on_call
            .lock()
            .map_err(|_| anyhow!("failure setting poisoned"))? = Some(7);
        assert!(chronicle.ask(question).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn ask_for_passes_the_callers_visibility_scope_to_retrieval() -> Result<()> {
        let (chronicle, retriever, _) = service(FakeOutcome::Results, ["answer"], 100)?;
        assert_eq!(
            chronicle
                .ask_for(
                    "question",
                    crate::chronicle::indexer::db::repository::AccessScope::Gm,
                )
                .await?,
            "answer"
        );
        assert_eq!(
            retriever
                .accesses
                .lock()
                .map_err(|_| anyhow!("accesses poisoned"))?
                .as_slice(),
            &[crate::chronicle::indexer::db::repository::AccessScope::Gm]
        );
        Ok(())
    }

    #[tokio::test]
    async fn ask_short_circuits_non_result_outcomes() -> Result<()> {
        let cases = [
            (
                FakeOutcome::BadQuestion,
                "Please provide a non-empty question.",
            ),
            (FakeOutcome::CorpusEmpty, "Chronicle corpus is empty."),
            (
                FakeOutcome::NoResult,
                "No relevant Chronicle context was found.",
            ),
            (FakeOutcome::Error, "Chronicle retrieval failed."),
        ];
        for (outcome, expected) in cases {
            let (chronicle, _, llm) = service(outcome, [], 100)?;
            assert_eq!(chronicle.ask("question").await?, expected);
            assert!(
                llm.prompts
                    .lock()
                    .map_err(|_| anyhow!("prompts poisoned"))?
                    .is_empty()
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn ask_retries_long_answer_and_accepts_shorter_revision() -> Result<()> {
        let (chronicle, _, llm) = service(FakeOutcome::Results, ["too long", "short"], 5)?;
        assert_eq!(chronicle.ask("question").await?, "short");
        let prompts = llm
            .prompts
            .lock()
            .map_err(|_| anyhow!("prompts poisoned"))?;
        assert_eq!(prompts.len(), 2);
        assert!(prompts[1].contains("Draft answer:\ntoo long"));
        Ok(())
    }

    #[tokio::test]
    async fn ask_truncates_second_long_answer_to_unicode_character_limit() -> Result<()> {
        let (chronicle, _, _) = service(FakeOutcome::Results, ["first long", "éclair"], 2)?;
        assert_eq!(chronicle.ask("question").await?, "éc");
        Ok(())
    }

    #[tokio::test]
    async fn ask_propagates_prompt_token_count_failure() -> Result<()> {
        let runtime = GpuRuntime::new();
        runtime.begin_llm_load()?.commit_to_loaded()?;
        let retriever = Arc::new(FakeRetriever::new(FakeOutcome::Results));
        let mut model = FakeLlm::new(runtime.clone(), ["unused"]);
        model.fail_count = true;
        let llm = Arc::new(model);
        let chronicle =
            Chronicle::with_dependencies(retriever, llm, runtime, 5, 15, 0.8, 0.85, 2, 0.15, 100);
        assert!(
            chronicle
                .ask("question")
                .await
                .unwrap_err()
                .to_string()
                .contains("token counting failed")
        );
        Ok(())
    }

    #[tokio::test]
    async fn lifecycle_loads_and_unloads_both_dependencies() -> Result<()> {
        let runtime = GpuRuntime::new();
        let retriever = Arc::new(FakeRetriever::new(FakeOutcome::Results));
        let llm = Arc::new(FakeLlm::new(runtime.clone(), []));
        let chronicle = Chronicle::with_dependencies(
            retriever.clone(),
            llm.clone(),
            runtime,
            5,
            15,
            0.8,
            0.85,
            2,
            0.15,
            100,
        );
        chronicle.start_llm().await?;
        assert!(chronicle.is_llm_loaded()?);
        chronicle.stop_llm().await?;
        assert!(!chronicle.is_llm_loaded()?);
        assert_eq!(mutex_value(&retriever.loads)?, 1);
        assert_eq!(mutex_value(&retriever.unloads)?, 1);
        assert_eq!(mutex_value(&llm.loads)?, 1);
        assert_eq!(mutex_value(&llm.unloads)?, 1);
        Ok(())
    }

    #[tokio::test]
    async fn failed_llm_load_unloads_embedder_and_restores_runtime() -> Result<()> {
        let runtime = GpuRuntime::new();
        let retriever = Arc::new(FakeRetriever::new(FakeOutcome::Results));
        let mut model = FakeLlm::new(runtime.clone(), []);
        model.fail_load = true;
        let llm = Arc::new(model);
        let chronicle = Chronicle::with_dependencies(
            retriever.clone(),
            llm,
            runtime.clone(),
            5,
            15,
            0.8,
            0.85,
            2,
            0.15,
            100,
        );
        assert!(chronicle.start_llm().await.is_err());
        assert_eq!(mutex_value(&retriever.loads)?, 1);
        assert_eq!(mutex_value(&retriever.unloads)?, 1);
        assert!(!runtime.is_llm_loaded()?);
        assert!(runtime.acquire_transcription().is_ok());
        Ok(())
    }
}
