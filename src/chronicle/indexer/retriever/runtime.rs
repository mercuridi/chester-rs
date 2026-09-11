use std::sync::Mutex;

use anyhow::{Context, Result, anyhow};
use candle_core::Device;
use tracing::{debug, info, instrument};

use super::{
    api::{RetrievalOutcome, RetrieverApi},
    pipeline::select_with_diagnostics_and_pagerank,
    settings::SearchSettings,
};
use crate::chronicle::indexer::{
    db::repository::facade::{AccessScope, IndexerDb},
    embedder::Embedder,
};

pub struct Retriever {
    db: IndexerDb,
    embedder: Mutex<Option<Embedder>>,
}

impl Retriever {
    pub fn new(db: IndexerDb) -> Self {
        Self {
            db,
            embedder: Mutex::new(None),
        }
    }

    #[instrument(skip(self))]
    pub async fn load_embedder(&self) -> Result<()> {
        let embedder = tokio::task::spawn_blocking(|| Embedder::load(Device::Cpu))
            .await
            .context("CPU embedder loading task failed")??;
        let mut slot = self
            .embedder
            .lock()
            .map_err(|_| anyhow!("Retriever embedder state is poisoned"))?;
        if slot.is_some() {
            debug!("Retriever embedder already loaded");
            return Ok(());
        }
        *slot = Some(embedder);
        info!("Retriever embedder loaded");
        Ok(())
    }

    pub fn unload_embedder(&self) -> Result<()> {
        let mut slot = self
            .embedder
            .lock()
            .map_err(|_| anyhow!("Retriever embedder state is poisoned"))?;
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
        let query = query.trim();
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
        let embedding = {
            let slot = self
                .embedder
                .lock()
                .map_err(|_| anyhow!("Retriever embedder state is poisoned"))?;
            let embedder = slot.as_ref().ok_or_else(|| {
                anyhow!("Chronicle retriever is not ready; run /chronicle start first")
            })?;
            embedder
                .embed(query)
                .with_context(|| "Failed to embed search query")?
        };
        let (vector, lexical) = tokio::try_join!(
            self.db
                .search_similar_for(&embedding, settings.limits.candidate_limit, access),
            self.db
                .search_lexical_for(query, settings.limits.candidate_limit, access),
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
    async fn load_embedder(&self) -> Result<()> {
        self.load_embedder().await
    }
    fn unload_embedder(&self) -> Result<()> {
        self.unload_embedder()
    }
}
