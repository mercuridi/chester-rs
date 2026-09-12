use anyhow::Result;

use super::SearchSettings;
use crate::chronicle::indexer::db::{AccessScope, SearchResult};

#[async_trait::async_trait]
pub trait RetrieverApi: Send + Sync {
    async fn search(
        &self,
        query: &str,
        settings: SearchSettings,
        access: AccessScope,
    ) -> Result<RetrievalOutcome>;
    /// Load the embedder, returning whether this call created it.
    async fn load_embedder(&self) -> Result<bool>;
    async fn unload_embedder(&self) -> Result<()>;
}

#[derive(Debug)]
pub enum RetrievalOutcome {
    Results(Vec<SearchResult>),
    BadQuestion,
    CorpusEmpty,
    NoResultMeetsThreshold,
}
