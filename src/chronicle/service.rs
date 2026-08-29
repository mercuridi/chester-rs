use anyhow::Result;
use tracing::{debug, info, instrument};

use super::{
    indexer::{
        db::repository::IndexerDb,
        prompt,
        retriever::{RetrievalOutcome, Retriever},
    },
    llm::Llm,
    runtime::GpuRuntime,
    transcription::service::TranscriptionService,
};

pub struct Chronicle {
    retriever: Retriever,
    llm: Llm,
    runtime: GpuRuntime,
    transcription: TranscriptionService,
    retrieval_limit: usize,
    retrieval_candidate_limit: usize,
    retrieval_distance_threshold: f32,
    max_reply_length: usize,
    lifecycle: tokio::sync::Mutex<()>,
}

impl Chronicle {
    pub fn new(
        db: IndexerDb,
        llm: Llm,
        runtime: GpuRuntime,
        retrieval_limit: usize,
        retrieval_candidate_limit: usize,
        retrieval_distance_threshold: f32,
        max_reply_length: usize,
    ) -> Self {
        Self {
            retriever: Retriever::new(db),
            llm,
            runtime: runtime.clone(),
            transcription: TranscriptionService::new(runtime),
            retrieval_limit,
            retrieval_candidate_limit,
            retrieval_distance_threshold,
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

        let prompt = prompt::build_prompt(question, &results);
        debug!(
            result_count = results.len(),
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
