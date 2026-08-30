use anyhow::Result;
use std::sync::Arc;
use tracing::{debug, info, instrument};

use super::{
    indexer::{
        db::repository::IndexerDb,
        prompt,
        retriever::{RetrievalOutcome, Retriever, RetrieverApi},
    },
    llm::{LanguageModel, Llm},
    runtime::GpuRuntime,
    transcription::service::TranscriptionService,
};

pub struct Chronicle {
    retriever: Arc<dyn RetrieverApi>,
    llm: Arc<dyn LanguageModel>,
    runtime: GpuRuntime,
    transcription: TranscriptionService,
    retrieval_limit: usize,
    retrieval_candidate_limit: usize,
    retrieval_distance_threshold: f32,
    retrieval_near_duplicate_threshold: f32,
    retrieval_max_chunks_per_document: usize,
    max_reply_length: usize,
    lifecycle: tokio::sync::Mutex<()>,
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
        max_reply_length: usize,
    ) -> Self {
        Self {
            retriever: Arc::new(Retriever::new(db)),
            llm: Arc::new(llm),
            runtime: runtime.clone(),
            transcription: TranscriptionService::new(runtime),
            retrieval_limit,
            retrieval_candidate_limit,
            retrieval_distance_threshold,
            retrieval_near_duplicate_threshold,
            retrieval_max_chunks_per_document,
            max_reply_length,
            lifecycle: tokio::sync::Mutex::new(()),
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
        max_reply_length: usize,
    ) -> Self {
        Self {
            retriever,
            llm,
            runtime: runtime.clone(),
            transcription: TranscriptionService::new(runtime),
            retrieval_limit,
            retrieval_candidate_limit,
            retrieval_distance_threshold,
            retrieval_near_duplicate_threshold,
            retrieval_max_chunks_per_document,
            max_reply_length,
            lifecycle: tokio::sync::Mutex::new(()),
        }
    }

    #[instrument(skip(self, question), fields(question_len = question.len()))]
    pub async fn ask(&self, question: &str) -> Result<String> {
        info!("Starting Chronicle question");
        let _lifecycle = self.lifecycle.lock().await;
        let _gpu_lease = self.runtime.acquire_inference()?;

        let outcome = match self
            .retriever
            .search(
                question,
                self.retrieval_limit,
                self.retrieval_candidate_limit,
                self.retrieval_distance_threshold,
                self.retrieval_near_duplicate_threshold,
                self.retrieval_max_chunks_per_document,
            )
            .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                tracing::warn!(%error, "Chronicle retrieval failed");
                return Ok("Chronicle retrieval failed.".to_owned());
            }
        };

        let results = match outcome {
            RetrievalOutcome::Results(results) => results,
            RetrievalOutcome::BadQuestion => {
                return Ok("Please provide a non-empty question.".to_owned());
            }
            RetrievalOutcome::CorpusEmpty => {
                return Ok("Chronicle corpus is empty.".to_owned());
            }
            RetrievalOutcome::NoResultMeetsThreshold => {
                return Ok("No relevant Chronicle context was found.".to_owned());
            }
        };

        let assembly = prompt::build_prompt_with_budget(
            question,
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

        let mut answer = self.llm.generate(&prompt).await?;

        if answer.chars().count() > self.max_reply_length {
            debug!(
                answer_len = answer.chars().count(),
                max_reply_length = self.max_reply_length,
                "LLM answer exceeded configured length; requesting a shorter answer"
            );
            let retry_prompt = format!(
                "{prompt}\n\nThe draft answer below is too long. Rewrite it to fit within {} characters. Preserve the most important information, and output only the shorter answer.\n\nDraft answer:\n{answer}",
                self.max_reply_length
            );
            answer = self.llm.generate(&retry_prompt).await?;
        }

        if answer.chars().count() > self.max_reply_length {
            tracing::warn!(
                answer_len = answer.chars().count(),
                max_reply_length = self.max_reply_length,
                "LLM answer remained over length after retry; truncating"
            );
            answer = truncate_to_char_limit(&answer, self.max_reply_length);
        }
        info!(
            answer_len = answer.chars().count(),
            "Completed Chronicle question"
        );
        Ok(answer)
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
#[allow(clippy::type_complexity, clippy::unwrap_used)]
mod tests {
    use super::{Chronicle, truncate_to_char_limit};
    use crate::chronicle::{
        indexer::{
            db::repository::SearchResult,
            retriever::{RetrievalOutcome, RetrieverApi},
        },
        llm::LanguageModel,
        runtime::GpuRuntime,
    };
    use anyhow::{Result, anyhow};
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };

    #[derive(Clone, Copy)]
    enum FakeOutcome {
        Results,
        BadQuestion,
        CorpusEmpty,
        NoResult,
        Error,
    }

    struct FakeRetriever {
        outcome: FakeOutcome,
        calls: Mutex<Vec<(String, usize, usize, f32, f32, usize)>>,
        loads: Mutex<usize>,
        unloads: Mutex<usize>,
    }

    impl FakeRetriever {
        fn new(outcome: FakeOutcome) -> Self {
            Self {
                outcome,
                calls: Mutex::new(Vec::new()),
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
            limit: usize,
            candidate_limit: usize,
            distance_threshold: f32,
            near_duplicate_threshold: f32,
            max_chunks_per_document: usize,
        ) -> Result<RetrievalOutcome> {
            self.calls
                .lock()
                .map_err(|_| anyhow!("calls poisoned"))?
                .push((
                    query.into(),
                    limit,
                    candidate_limit,
                    distance_threshold,
                    near_duplicate_threshold,
                    max_chunks_per_document,
                ));
            match self.outcome {
                FakeOutcome::Results => Ok(RetrievalOutcome::Results(vec![SearchResult {
                    document_path: "doc.md".into(),
                    chunk_index: 0,
                    heading: None,
                    text: "context".into(),
                    overlaps_previous: false,
                    distance: 0.1,
                }])),
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
        budget: usize,
        fail_count: bool,
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
                budget: 10_000,
                fail_count: false,
                fail_load: false,
                loads: Mutex::new(0),
                unloads: Mutex::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl LanguageModel for FakeLlm {
        fn prompt_token_budget(&self) -> usize {
            self.budget
        }

        fn count_input_tokens(&self, prompt: &str) -> Result<usize> {
            if self.fail_count {
                return Err(anyhow!("token counting failed"));
            }
            Ok(prompt.chars().count())
        }

        async fn generate(&self, prompt: &str) -> Result<String> {
            self.prompts
                .lock()
                .map_err(|_| anyhow!("prompts poisoned"))?
                .push(prompt.into());
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
        assert!(prompts[0].contains("Document: doc.md"));
        assert!(prompts[0].contains("Question:\nquestion"));
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
            Chronicle::with_dependencies(retriever, llm, runtime, 5, 15, 0.8, 0.85, 2, 100);
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
