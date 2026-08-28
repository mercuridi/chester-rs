use anyhow::Result;

use super::{
    indexer::{
        db::repository::IndexerDb,
        embedder::Embedder,
        prompt,
        retriever::Retriever,
    },
    llm::Llm,
    runtime::GpuRuntime,
};

pub struct Chronicle {
    retriever: Retriever,
    llm: Llm,
    runtime: GpuRuntime,
    retrieval_limit: usize,
}

impl Chronicle {
    pub fn new(
        db: IndexerDb,
        embedder: Embedder,
        llm: Llm,
        runtime: GpuRuntime,
        retrieval_limit: usize,
    ) -> Self {
        Self {
            retriever: Retriever::new(db, embedder),
            llm,
            runtime,
            retrieval_limit,
        }
    }

    pub async fn ask(&self, question: &str) -> Result<String> {
        let _gpu_lease = self.runtime.acquire_inference()?;

        let results = self.retriever.search(question, self.retrieval_limit).await?;

        let prompt = prompt::build_prompt(question, &results);

        self.llm.generate(&prompt).await
    }

    pub fn runtime(&self) -> GpuRuntime {
        self.runtime.clone()
    }

    pub async fn start_llm(&self) -> Result<()> {
        self.llm.load().await
    }

    pub async fn stop_llm(&self) -> Result<()> {
        self.llm.unload().await
    }

    pub fn is_llm_loaded(&self) -> Result<bool> {
        self.runtime.is_llm_loaded()
    }
}
