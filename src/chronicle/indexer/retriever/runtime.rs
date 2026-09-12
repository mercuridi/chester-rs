use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use candle_core::Device;
use tokio::sync::{RwLock, Semaphore};
use tracing::{debug, info, instrument};

use super::{
    api::{RetrievalOutcome, RetrieverApi},
    pipeline::select_with_diagnostics_and_pagerank,
    settings::SearchSettings,
};
use crate::chronicle::indexer::{
    db::{AccessScope, IndexerDb},
    embedder::Embedder,
};

const EMBEDDING_WORKER_LIMIT: usize = 2;

pub struct Retriever {
    db: IndexerDb,
    embedder: RwLock<Option<Arc<Embedder>>>,
    embedding_workers: Arc<Semaphore>,
}

impl Retriever {
    pub fn new(db: IndexerDb) -> Self {
        Self {
            db,
            embedder: RwLock::new(None),
            embedding_workers: Arc::new(Semaphore::new(EMBEDDING_WORKER_LIMIT)),
        }
    }

    #[instrument(skip(self))]
    pub async fn load_embedder(&self) -> Result<bool> {
        let embedder = tokio::task::spawn_blocking(|| Embedder::load(Device::Cpu))
            .await
            .context("CPU embedder loading task failed")??;
        let mut slot = self.embedder.write().await;
        if slot.is_some() {
            debug!("Retriever embedder already loaded");
            return Ok(false);
        }
        *slot = Some(Arc::new(embedder));
        info!("Retriever embedder loaded");
        Ok(true)
    }

    pub async fn unload_embedder(&self) -> Result<()> {
        let mut slot = self.embedder.write().await;
        slot.take();
        info!("Retriever embedder unloaded");
        Ok(())
    }

    #[instrument(skip(self, query), fields(query_len = query.len(), limit, candidate_limit, distance_threshold, near_duplicate_threshold, max_chunks_per_document))]
    pub async fn search(
        &self,
        query: &str,
        settings: SearchSettings,
        access: AccessScope,
    ) -> Result<RetrievalOutcome> {
        let query = query.trim().to_owned();
        if query.is_empty() {
            return Ok(RetrievalOutcome::BadQuestion);
        }
        if !self
            .db
            .has_chunks()
            .await
            .context("Failed to inspect Chronicle corpus")?
        {
            return Ok(RetrievalOutcome::CorpusEmpty);
        }
        let embedder = self.embedder.read().await.clone().ok_or_else(|| {
            anyhow!("Chronicle retriever is not ready; run /chronicle start first")
        })?;
        let worker_permit = Arc::clone(&self.embedding_workers)
            .acquire_owned()
            .await
            .context("Failed to acquire CPU query embedding worker")?;
        let embedding_query = query.clone();
        let embedding = tokio::task::spawn_blocking(move || {
            let _worker_permit = worker_permit;
            embedder.embed(&embedding_query)
        })
        .await
        .context("CPU query embedding task failed")?
        .with_context(|| "Failed to embed search query")?;
        let (vector, lexical) = tokio::try_join!(
            self.db
                .search_similar_for(&embedding, settings.limits.candidate_limit, access),
            self.db
                .search_lexical_for(&query, settings.limits.candidate_limit, access),
        )?;
        let paths = vector
            .iter()
            .chain(&lexical)
            .map(|result| result.document_path.clone())
            .collect::<Vec<_>>();
        let pagerank = self.db.pagerank_for_paths(paths, access).await?;
        let (results, diagnostics) =
            select_with_diagnostics_and_pagerank(vector, lexical, settings, &pagerank);
        debug!(?diagnostics, "Chronicle retrieval diagnostics");
        if results.is_empty() {
            Ok(RetrievalOutcome::NoResultMeetsThreshold)
        } else {
            Ok(RetrievalOutcome::Results(results))
        }
    }
}

#[async_trait::async_trait]
impl RetrieverApi for Retriever {
    async fn search(
        &self,
        query: &str,
        settings: SearchSettings,
        access: AccessScope,
    ) -> Result<RetrievalOutcome> {
        self.search(query, settings, access).await
    }
    async fn load_embedder(&self) -> Result<bool> {
        self.load_embedder().await
    }
    async fn unload_embedder(&self) -> Result<()> {
        self.unload_embedder().await
    }
}
