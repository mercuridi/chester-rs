use anyhow::Result;
use tracing::{debug, info, instrument};

use super::{
    indexer::{db::repository::IndexerDb, prompt, retriever::Retriever},
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
    lifecycle: tokio::sync::Mutex<()>,
}

impl Chronicle {
    pub fn new(db: IndexerDb, llm: Llm, runtime: GpuRuntime, retrieval_limit: usize) -> Self {
        Self {
            retriever: Retriever::new(db),
            llm,
            runtime: runtime.clone(),
            transcription: TranscriptionService::new(runtime),
            retrieval_limit,
            lifecycle: tokio::sync::Mutex::new(()),
        }
    }

    #[instrument(skip(self, question), fields(question_len = question.len()))]
    pub async fn ask(&self, question: &str) -> Result<String> {
        info!("Starting Chronicle question");
        let _lifecycle = self.lifecycle.lock().await;
        let _gpu_lease = self.runtime.acquire_inference()?;

        let results = self
            .retriever
            .search(question, self.retrieval_limit)
            .await?;

        let prompt = prompt::build_prompt(question, &results);
        debug!(
            result_count = results.len(),
            prompt_len = prompt.len(),
            "Built Chronicle prompt"
        );

        let answer = self.llm.generate(&prompt).await?;
        info!(answer_len = answer.len(), "Completed Chronicle question");
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
