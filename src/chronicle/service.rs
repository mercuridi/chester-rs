use anyhow::Result;

use super::{
    indexer::{
        db::repository::IndexerDb,
        embedder::Embedder,
        prompt,
        retriever::Retriever,
    },
    llm::Llm,
};

pub struct Chronicle {
    retriever: Retriever,
    llm: Llm,
}

impl Chronicle {
    pub fn new(
        db: IndexerDb,
        embedder: Embedder,
        llm: Llm,
    ) -> Self {
        Self {
            retriever: Retriever::new(db, embedder),
            llm,
        }
    }

    pub async fn ask(&self, question: &str) -> Result<String> {
        let results = self.retriever.search(question, 5).await?;

        let prompt = prompt::build_prompt(question, &results);

        self.llm.generate(&prompt).await
    }
}