use anyhow::Result;

use super::settings::SearchSettings;
use crate::chronicle::indexer::db::repository::facade::{AccessScope, SearchResult};

#[async_trait::async_trait]
pub trait RetrieverApi: Send + Sync {
    async fn search(
        &self,
        query: &str,
        settings: SearchSettings,
        access: AccessScope,
    ) -> Result<RetrievalOutcome>;
    async fn load_embedder(&self) -> Result<()>;
    async fn unload_embedder(&self) -> Result<()>;
}

#[derive(Debug)]
pub enum RetrievalOutcome {
    Results(Vec<SearchResult>),
    BadQuestion,
    CorpusEmpty,
    NoResultMeetsThreshold,
}
