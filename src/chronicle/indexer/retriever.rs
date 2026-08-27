use anyhow::{Context, Result};

use super::{
    db::repository::{IndexerDb, SearchResult},
    embedder::Embedder,
};

pub struct Retriever {
    db: IndexerDb,
    embedder: Embedder,
}

impl Retriever {
    pub fn new(db: IndexerDb, embedder: Embedder) -> Self {
        Self { db, embedder }
    }

    pub async fn search(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let query = query.trim();

        if query.is_empty() {
            return Ok(Vec::new());
        }

        let embedding = self
            .embedder
            .embed(query)
            .with_context(|| "Failed to embed search query")?;

        self.db
            .search_similar(&embedding, limit)
            .await
            .context("Failed to search index")
    }
}