use super::{
    answer_routing::{self, AnswerRoute, RetrievalMode},
    lifecycle, retrieval_answer,
    synthesis_pipeline::{self, EvidenceRetrieval},
};
use anyhow::Result;
use serde::Serialize;
use std::sync::Arc;
use tracing::{debug, info, instrument};

use super::super::{
    config::chronicle::{GenerationSettings, RetrievalSettings, SynthesisSettings},
    indexer::{
        db::repository::facade::{AccessScope, IndexerDb, SearchResult, StructuredResult},
        retriever::api::RetrieverApi,
    },
    llm::LanguageModel,
    query::{
        plan::{RouteOperation, StructuredPlan},
        render,
    },
    runtime::GpuRuntime,
    synthesis::{self, EvidenceNote},
    transcription::service::TranscriptionService,
};

pub use super::synthesis_pipeline::SynthesisDiagnostics;

pub struct Chronicle {
    retriever: Arc<dyn RetrieverApi>,
    structured_store: Arc<dyn StructuredStore>,
    llm: Arc<dyn LanguageModel>,
    runtime: GpuRuntime,
    transcription: TranscriptionService,
    retrieval: RetrievalSettings,
    synthesis: SynthesisSettings,
    generation: GenerationSettings,
    lifecycle: tokio::sync::Mutex<()>,
}

#[async_trait::async_trait]
pub trait StructuredStore: Send + Sync {
    async fn resolve_string_or_wikilinks(
        &self,
        plan: &mut StructuredPlan,
        access: AccessScope,
    ) -> Result<()>;

    async fn execute_plan(
        &self,
        plan: &StructuredPlan,
        access: AccessScope,
    ) -> Result<StructuredResult>;
}

#[async_trait::async_trait]
impl StructuredStore for IndexerDb {
    async fn resolve_string_or_wikilinks(
        &self,
        plan: &mut StructuredPlan,
        access: AccessScope,
    ) -> Result<()> {
        self.resolve_string_or_wikilinks(plan, access).await
    }

    async fn execute_plan(
        &self,
        plan: &StructuredPlan,
        access: AccessScope,
    ) -> Result<StructuredResult> {
        self.execute_plan_for(plan, access).await
    }
}

pub struct ChronicleDependencies {
    pub retriever: Arc<dyn RetrieverApi>,
    pub structured_store: Arc<dyn StructuredStore>,
    pub llm: Arc<dyn LanguageModel>,
    pub runtime: GpuRuntime,
    pub transcription: TranscriptionService,
}

struct SynthesisEvidenceNotes {
    notes: Vec<EvidenceNote>,
    partial: bool,
    reduction_depth: usize,
}

const PARTIAL_SYNTHESIS_PREFIX: &str =
    "This is a partial synthesis based on the retrieved notes completed so far.\n\n";

/// The route that actually generated a Chronicle reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectiveRoute {
    Structured,
    Retrieval,
    Synthesis,
    Clarification,
    EmptyQuestion,
}

/// Per-request metadata for a Chronicle answer. This is deliberately returned with the answer
/// rather than retained as mutable service state, so evaluators cannot associate one request's
/// route or diagnostics with another request's reply.
#[derive(Debug, Clone, Serialize)]
pub struct ChronicleAnswer {
    pub reply: String,
    pub classifier_response: Option<String>,
    pub classifier_error: Option<String>,
    pub classified_operation: Option<RouteOperation>,
    pub effective_route: EffectiveRoute,
    pub synthesis_diagnostics: Option<SynthesisDiagnostics>,
}

/// The common boundary returned by every answer route.
struct AnswerOutcome {
    reply: String,
    synthesis_diagnostics: Option<SynthesisDiagnostics>,
}

impl AnswerOutcome {
    fn new(reply: String) -> Self {
        Self {
            reply,
            synthesis_diagnostics: None,
        }
    }

    fn bounded(reply: &str, max_reply_length: usize) -> Self {
        Self::new(reply.chars().take(max_reply_length).collect())
    }
}

impl Chronicle {
    pub fn new(
        retrieval: RetrievalSettings,
        synthesis: SynthesisSettings,
        generation: GenerationSettings,
        dependencies: ChronicleDependencies,
    ) -> Self {
        Self {
            retriever: dependencies.retriever,
            structured_store: dependencies.structured_store,
            llm: dependencies.llm,
            runtime: dependencies.runtime,
            transcription: dependencies.transcription,
            retrieval,
            synthesis,
            generation,
            lifecycle: tokio::sync::Mutex::new(()),
        }
    }

    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "convenience API for player-scoped callers")
    )]
    #[instrument(skip(self, question), fields(question_len = question.len()))]
    pub async fn ask(&self, question: &str) -> Result<String> {
        self.ask_for(question, AccessScope::Player).await
    }

    pub async fn ask_for(&self, question: &str, access: AccessScope) -> Result<String> {
        Ok(self.ask_for_with_metadata(question, access).await?.reply)
    }

    pub(crate) async fn ask_with_metadata(&self, question: &str) -> Result<ChronicleAnswer> {
        self.ask_for_with_metadata(question, AccessScope::Player)
            .await
    }

    pub(crate) async fn ask_for_with_metadata(
        &self,
        question: &str,
        access: AccessScope,
    ) -> Result<ChronicleAnswer> {
        info!("Starting Chronicle question");
        let _lifecycle = self.lifecycle.lock().await;

        let selection = self.select_answer_route(question, access).await?;
        let effective_route = Self::effective_route(&selection.route);
        let outcome = self
            .execute_answer_route(question, access, selection.route)
            .await?;
        info!(
            reply_len = outcome.reply.chars().count(),
            "Completed Chronicle question"
        );
        Ok(ChronicleAnswer {
            reply: outcome.reply,
            classifier_response: selection.classifier_response,
            classifier_error: selection.classifier_error,
            classified_operation: selection.classified_operation,
            effective_route,
            synthesis_diagnostics: outcome.synthesis_diagnostics,
        })
    }

    async fn select_answer_route(
        &self,
        question: &str,
        access: AccessScope,
    ) -> Result<answer_routing::RouteSelection> {
        answer_routing::select_answer_route(
            self.llm.as_ref(),
            self.structured_store.as_ref(),
            question,
            access,
        )
        .await
    }

    fn effective_route(route: &AnswerRoute) -> EffectiveRoute {
        match route {
            AnswerRoute::Structured(_) => EffectiveRoute::Structured,
            AnswerRoute::Retrieval(_) => EffectiveRoute::Retrieval,
            AnswerRoute::Synthesis => EffectiveRoute::Synthesis,
            AnswerRoute::Clarification => EffectiveRoute::Clarification,
            AnswerRoute::EmptyQuestion => EffectiveRoute::EmptyQuestion,
        }
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
                self.generation.max_reply_length,
            )),
            AnswerRoute::EmptyQuestion => Ok(AnswerOutcome::new(
                "Please provide a non-empty question.".into(),
            )),
        }
    }

    async fn answer_from_structured_plan(
        &self,
        plan: &StructuredPlan,
        access: AccessScope,
    ) -> Result<AnswerOutcome> {
        let result = self.structured_store.execute_plan(plan, access).await?;
        Ok(AnswerOutcome::new(render::render(
            plan,
            &result,
            self.generation.max_reply_length,
        )))
    }

    async fn answer_from_retrieval(
        &self,
        question: &str,
        mode: RetrievalMode,
        access: AccessScope,
    ) -> Result<AnswerOutcome> {
        retrieval_answer::answer_from_retrieval(
            self.retriever.as_ref(),
            self.llm.as_ref(),
            &self.retrieval,
            &self.generation,
            question,
            mode,
            access,
        )
        .await
        .map(AnswerOutcome::new)
    }

    async fn answer_from_synthesis(
        &self,
        question: &str,
        access: AccessScope,
    ) -> Result<AnswerOutcome> {
        let results = match synthesis_pipeline::retrieve_evidence(
            self.retriever.as_ref(),
            &self.retrieval,
            &self.synthesis,
            question,
            access,
        )
        .await
        {
            EvidenceRetrieval::Evidence(results) => results,
            EvidenceRetrieval::ImmediateResponse(response) => {
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
        Ok(AnswerOutcome {
            reply: answer,
            synthesis_diagnostics: Some(diagnostics),
        })
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
            self.generation
                .max_reply_length
                .saturating_sub(PARTIAL_SYNTHESIS_PREFIX.chars().count())
        } else {
            self.generation.max_reply_length
        };
        let (answer, retried, truncated) =
            retrieval_answer::generate_answer(self.llm.as_ref(), &final_prompt, answer_limit)
                .await?;
        diagnostics.final_answer_length_retried = retried;
        diagnostics.final_answer_truncated = truncated;
        Ok(if evidence.partial {
            format!("{PARTIAL_SYNTHESIS_PREFIX}{answer}")
        } else {
            answer
        })
    }

    fn incomplete_synthesis_response(&self) -> String {
        retrieval_answer::truncate_to_char_limit(
            "Chronicle synthesis could not be completed from the retrieved notes.",
            self.generation.max_reply_length,
        )
    }

    #[instrument(skip(self))]
    pub async fn start_llm(&self) -> Result<()> {
        let _lifecycle = self.lifecycle.lock().await;
        lifecycle::start(self.retriever.as_ref(), self.llm.as_ref()).await
    }

    #[instrument(skip(self))]
    pub async fn stop_llm(&self) -> Result<()> {
        let _lifecycle = self.lifecycle.lock().await;
        lifecycle::stop(self.retriever.as_ref(), self.llm.as_ref()).await
    }

    pub fn is_llm_loaded(&self) -> Result<bool> {
        self.runtime.is_llm_loaded()
    }

    pub fn transcription_service(&self) -> TranscriptionService {
        self.transcription.clone()
    }
}

#[cfg(test)]
#[allow(clippy::type_complexity, clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::super::retrieval_answer::truncate_to_char_limit;
    use super::{
        Chronicle, ChronicleDependencies, EffectiveRoute, GenerationSettings, RetrievalSettings,
        StructuredStore, SynthesisSettings,
    };
    use crate::chronicle::{
        indexer::{
            db::repository::facade::{AccessScope, IndexerDb, SearchResult, StructuredResult},
            retriever::{
                api::{RetrievalOutcome, RetrieverApi},
                settings::SearchSettings,
            },
        },
        llm::LanguageModel,
        query::plan::{RouteOperation, StructuredOperation, StructuredPlan},
        runtime::GpuRuntime,
        transcription::service::TranscriptionService,
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

    const SYNTHESIS_SETTINGS: SynthesisSettings = SynthesisSettings {
        retrieval_limit: 12,
        candidate_limit: 40,
        max_chunks_per_document: 3,
        batch_token_budget: 1_800,
        max_batches: 6,
    };

    struct FakeRetriever {
        outcome: FakeOutcome,
        calls: Mutex<Vec<(String, usize, usize, f32, f32, usize)>>,
        accesses: Mutex<Vec<crate::chronicle::indexer::db::repository::facade::AccessScope>>,
        loads: Mutex<usize>,
        unloads: Mutex<usize>,
    }

    struct FakeStructuredStore {
        db: Mutex<Option<IndexerDb>>,
    }

    impl FakeStructuredStore {
        fn new() -> Self {
            Self {
                db: Mutex::new(None),
            }
        }

        fn set_db(&self, db: IndexerDb) -> Result<()> {
            *self.db.lock().map_err(|_| anyhow!("database poisoned"))? = Some(db);
            Ok(())
        }

        fn database(&self) -> Result<IndexerDb> {
            self.db
                .lock()
                .map_err(|_| anyhow!("database poisoned"))?
                .clone()
                .ok_or_else(|| anyhow!("structured store not configured"))
        }
    }

    #[async_trait::async_trait]
    impl StructuredStore for FakeStructuredStore {
        async fn resolve_string_or_wikilinks(
            &self,
            plan: &mut StructuredPlan,
            access: AccessScope,
        ) -> Result<()> {
            self.database()?
                .resolve_string_or_wikilinks(plan, access)
                .await
        }

        async fn execute_plan(
            &self,
            plan: &StructuredPlan,
            access: AccessScope,
        ) -> Result<StructuredResult> {
            self.database()?.execute_plan_for(plan, access).await
        }
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
            _access: crate::chronicle::indexer::db::repository::facade::AccessScope,
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

        async fn unload_embedder(&self) -> Result<()> {
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
        classifier_output: Mutex<Option<String>>,
        plan_output: Mutex<String>,
        plan_calls: Mutex<usize>,
        structured_plan_calls: Mutex<usize>,
        repair_structured_plan_output: Mutex<Option<String>>,
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
                classifier_output: Mutex::new(None),
                plan_output: Mutex::new(r#"{"operation":"search"}"#.into()),
                plan_calls: Mutex::new(0),
                structured_plan_calls: Mutex::new(0),
                repair_structured_plan_output: Mutex::new(None),
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

        async fn classify_route(&self, question: &str) -> Result<String> {
            *self
                .plan_calls
                .lock()
                .map_err(|_| anyhow!("plan counter poisoned"))? += 1;
            if let Some(output) = self
                .classifier_output
                .lock()
                .map_err(|_| anyhow!("classifier output poisoned"))?
                .clone()
            {
                return Ok(output);
            }
            let plan = self
                .plan_output
                .lock()
                .map_err(|_| anyhow!("plan poisoned"))?
                .clone();
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&plan)
                && let Some(operation) = value.get("operation")
            {
                return Ok(serde_json::json!({ "operation": operation }).to_string());
            }
            let question = question.to_lowercase();
            let operation = if question.starts_with("list") || question.starts_with("name") {
                "list"
            } else if question.contains("how many") {
                "count"
            } else {
                "search"
            };
            Ok(serde_json::json!({ "operation": operation }).to_string())
        }

        async fn generate_structured_plan(
            &self,
            _question: &str,
            _operation: StructuredOperation,
        ) -> Result<String> {
            *self
                .structured_plan_calls
                .lock()
                .map_err(|_| anyhow!("structured plan counter poisoned"))? += 1;
            Ok(self
                .plan_output
                .lock()
                .map_err(|_| anyhow!("plan poisoned"))?
                .clone())
        }

        async fn repair_structured_plan(
            &self,
            question: &str,
            _operation: StructuredOperation,
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
            self.repair_structured_plan_output
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
        let structured_store = Arc::new(FakeStructuredStore::new());
        service_with_store(outcome, outputs, max_reply_length, structured_store)
    }

    fn service_with_store(
        outcome: FakeOutcome,
        outputs: impl IntoIterator<Item = &'static str>,
        max_reply_length: usize,
        structured_store: Arc<FakeStructuredStore>,
    ) -> Result<(Chronicle, Arc<FakeRetriever>, Arc<FakeLlm>)> {
        let runtime = GpuRuntime::new();
        runtime.begin_llm_load()?.commit_to_loaded()?;
        let retriever = Arc::new(FakeRetriever::new(outcome));
        let llm = Arc::new(FakeLlm::new(runtime.clone(), outputs));
        let chronicle = Chronicle::new(
            RetrievalSettings {
                limit: 5,
                candidate_limit: 15,
                distance_threshold: 0.8,
                near_duplicate_threshold: 0.85,
                max_chunks_per_document: 2,
                pagerank_weight: 0.15,
            },
            SYNTHESIS_SETTINGS,
            GenerationSettings {
                max_tokens: 1,
                context_limit: 2,
                temperature: 0.0,
                seed: 0,
                system_prompt: "test".into(),
                max_reply_length,
            },
            ChronicleDependencies {
                retriever: retriever.clone(),
                structured_store,
                llm: llm.clone(),
                runtime: runtime.clone(),
                transcription: TranscriptionService::new(runtime),
            },
        );
        Ok((chronicle, retriever, llm))
    }

    fn mutex_value(value: &Mutex<usize>) -> Result<usize> {
        value
            .lock()
            .map(|guard| *guard)
            .map_err(|_| anyhow!("counter poisoned"))
    }

    fn chronicle_with_dependencies(
        retriever: Arc<dyn RetrieverApi>,
        llm: Arc<dyn LanguageModel>,
        runtime: GpuRuntime,
        max_reply_length: usize,
    ) -> Chronicle {
        Chronicle::new(
            RetrievalSettings {
                limit: 5,
                candidate_limit: 15,
                distance_threshold: 0.8,
                near_duplicate_threshold: 0.85,
                max_chunks_per_document: 2,
                pagerank_weight: 0.15,
            },
            SYNTHESIS_SETTINGS,
            GenerationSettings {
                max_tokens: 1,
                context_limit: 2,
                temperature: 0.0,
                seed: 0,
                system_prompt: "test".into(),
                max_reply_length,
            },
            ChronicleDependencies {
                retriever,
                structured_store: Arc::new(FakeStructuredStore::new()),
                llm,
                transcription: TranscriptionService::new(runtime.clone()),
                runtime,
            },
        )
    }

    #[tokio::test]
    async fn structured_zero_and_list_bypass_retrieval_and_answer_generation() -> Result<()> {
        let structured_store = Arc::new(FakeStructuredStore::new());
        let (chronicle, retriever, llm) =
            service_with_store(FakeOutcome::Error, [], 500, structured_store.clone())?;
        let directory = tempfile::tempdir()?;
        let db = IndexerDb::open(&format!(
            "sqlite://{}",
            directory.path().join("test.sqlite3").display()
        ))
        .await?;
        let (metadata, _) = crate::chronicle::indexer::frontmatter::parse("---\nid: ada\ntype: character\nstatus: canon\nvisibility: player\ncreated: 2026-09-07\nupdated: 2026-09-07\nrole: npc\nlife_status: alive\n---\n")?.context("note")?;
        db.replace_note("Ada.md", "hash", &[], &[], &metadata)
            .await?;
        structured_store.set_db(db)?;
        *llm.plan_output.lock().map_err(|_| anyhow!("plan poisoned"))? = r#"{"operation":"count","note_type":"character","filters":{"conditions":[{"field":"role","operator":"equals","value":"pc"},{"field":"life_status","operator":"equals","value":"dead"}]}}"#.into();
        let answer = chronicle.ask("How many dead PCs?").await?;
        assert!(answer.starts_with("0 canon characters recorded"));
        *llm.plan_output
            .lock()
            .map_err(|_| anyhow!("plan poisoned"))? =
            r#"{"operation":"list","note_type":"character","filters":{"conditions":[{"field":"role","operator":"equals","value":"npc"}]}}"#.into();
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
        let structured_store = Arc::new(FakeStructuredStore::new());
        let (chronicle, retriever, llm) =
            service_with_store(FakeOutcome::Error, [], 500, structured_store.clone())?;
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
        structured_store.set_db(db)?;
        *llm.plan_output
            .lock()
            .map_err(|_| anyhow!("plan poisoned"))? = "This is not a JSON query plan.".into();
        *llm
            .repair_structured_plan_output
            .lock()
            .map_err(|_| anyhow!("repair plan poisoned"))? = Some(r#"{"operation":"list","note_type":"character","filters":{"conditions":[{"field":"role","operator":"equals","value":"pc"},{"field":"played_by","operator":"equals","value":"Rowan"}]}}"#.into());

        let answer = chronicle.ask("List all PCs played by Rowan.").await?;

        assert!(answer.starts_with("2 canon characters recorded"));
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
    async fn invalid_route_classification_uses_best_effort_retrieval_without_planning() -> Result<()>
    {
        let (chronicle, retriever, llm) =
            service(FakeOutcome::Results, ["Some documented examples."], 500)?;
        *llm.classifier_output
            .lock()
            .map_err(|_| anyhow!("classifier output poisoned"))? = Some("not JSON".into());

        let answer = chronicle
            .ask_with_metadata("How many characters are in Northmere?")
            .await?;

        assert!(
            answer
                .reply
                .starts_with("Chronicle couldn't classify this request")
        );
        assert_eq!(answer.effective_route, EffectiveRoute::Retrieval);
        assert_eq!(answer.classified_operation, None);
        assert_eq!(answer.classifier_response.as_deref(), Some("not JSON"));
        assert!(answer.classifier_error.is_some());
        assert!(answer.synthesis_diagnostics.is_none());
        assert!(
            llm.prompts
                .lock()
                .map_err(|_| anyhow!("prompts poisoned"))?[0]
                .contains("could not classify this request")
        );
        assert_eq!(mutex_value(&llm.structured_plan_calls)?, 0);
        assert!(
            llm.repair_requests
                .lock()
                .map_err(|_| anyhow!("repair requests poisoned"))?
                .is_empty()
        );
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

    #[tokio::test]
    async fn definitely_unsupported_structured_request_skips_planner() -> Result<()> {
        let (chronicle, _retriever, llm) =
            service(FakeOutcome::Results, ["Some documented examples."], 500)?;

        let answer = chronicle.ask("List characters who are not dead.").await?;

        assert!(answer.starts_with("An exhaustive count or list is unavailable"));
        assert_eq!(mutex_value(&llm.plan_calls)?, 0);
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

        let answer = chronicle
            .ask_with_metadata("Summarise the history of Northmere.")
            .await?;
        assert_eq!(answer.reply, "answer");
        assert_eq!(answer.classified_operation, Some(RouteOperation::Synthesis));
        assert_eq!(answer.effective_route, EffectiveRoute::Synthesis);
        assert!(answer.synthesis_diagnostics.is_some());
        assert_eq!(mutex_value(&llm.plan_calls)?, 1);
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
    async fn answer_metadata_does_not_reuse_synthesis_diagnostics_for_a_later_retrieval()
    -> Result<()> {
        let (chronicle, _retriever, llm) = service(
            FakeOutcome::Results,
            ["evidence note", "synthesis answer", "retrieval answer"],
            100,
        )?;
        *llm.plan_output
            .lock()
            .map_err(|_| anyhow!("plan poisoned"))? = r#"{"operation":"synthesis"}"#.into();

        let synthesis = chronicle.ask_with_metadata("Summarise Northmere.").await?;
        assert_eq!(synthesis.effective_route, EffectiveRoute::Synthesis);
        assert!(synthesis.synthesis_diagnostics.is_some());

        *llm.plan_output
            .lock()
            .map_err(|_| anyhow!("plan poisoned"))? = r#"{"operation":"search"}"#.into();
        let retrieval = chronicle
            .ask_with_metadata("Tell me about Northmere.")
            .await?;
        assert_eq!(retrieval.reply, "retrieval answer");
        assert_eq!(retrieval.effective_route, EffectiveRoute::Retrieval);
        assert!(retrieval.synthesis_diagnostics.is_none());
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
                    crate::chronicle::indexer::db::repository::facade::AccessScope::Gm,
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
            &[crate::chronicle::indexer::db::repository::facade::AccessScope::Gm]
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
                    .map(|index| super::super::super::synthesis::EvidenceNote {
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
                    .map(|index| super::super::super::synthesis::EvidenceNote {
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
                    crate::chronicle::indexer::db::repository::facade::AccessScope::Gm,
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
            &[crate::chronicle::indexer::db::repository::facade::AccessScope::Gm]
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
        let chronicle = chronicle_with_dependencies(retriever, llm, runtime, 100);
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
        let chronicle = chronicle_with_dependencies(retriever.clone(), llm.clone(), runtime, 100);
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
        let chronicle = chronicle_with_dependencies(retriever.clone(), llm, runtime.clone(), 100);
        assert!(chronicle.start_llm().await.is_err());
        assert_eq!(mutex_value(&retriever.loads)?, 1);
        assert_eq!(mutex_value(&retriever.unloads)?, 1);
        assert!(!runtime.is_llm_loaded()?);
        assert!(runtime.acquire_transcription().is_ok());
        Ok(())
    }
}
