use anyhow::Result;

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

    pub async fn ask(&self, question: &str) -> Result<String> {
        let _lifecycle = self.lifecycle.lock().await;
        let _gpu_lease = self.runtime.acquire_inference()?;

        let results = self
            .retriever
            .search(question, self.retrieval_limit)
            .await?;

        let prompt = prompt::build_prompt(question, &results);

        self.llm.generate(&prompt).await
    }

    pub async fn start_llm(&self) -> Result<()> {
        let _lifecycle = self.lifecycle.lock().await;

        self.retriever.load_embedder().await?;

        if let Err(error) = self.llm.load().await {
            self.retriever.unload_embedder()?;
            return Err(error);
        }

        Ok(())
    }

    pub async fn stop_llm(&self) -> Result<()> {
        let _lifecycle = self.lifecycle.lock().await;

        self.llm.unload().await?;
        self.retriever.unload_embedder()?;
        Ok(())
    }

    pub fn is_llm_loaded(&self) -> Result<bool> {
        self.runtime.is_llm_loaded()
    }

    pub fn transcription_service(&self) -> TranscriptionService {
        self.transcription.clone()
    }
}
