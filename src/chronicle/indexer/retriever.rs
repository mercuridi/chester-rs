use std::sync::Mutex;

use anyhow::{Context, Result, anyhow};
use candle_core::Device;
use tracing::{debug, info, instrument};

use super::{
    db::repository::{IndexerDb, SearchResult},
    embedder::Embedder,
};

#[derive(Debug)]
pub enum RetrievalOutcome {
    Results(Vec<SearchResult>),
    BadQuestion,
    CorpusEmpty,
    NoResultMeetsThreshold,
}

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

    #[instrument(skip(self, query), fields(query_len = query.len(), limit, candidate_limit, distance_threshold))]
    pub async fn search(
        &self,
        query: &str,
        limit: usize,
        candidate_limit: usize,
        distance_threshold: f32,
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

        self.db
            .search_similar(&embedding, candidate_limit)
            .await
            .context("Failed to search index")
            .map(|results| {
                let results = results
                    .into_iter()
                    .filter(|result| result.distance <= distance_threshold)
                    .take(limit)
                    .collect::<Vec<_>>();

                debug!(
                    result_count = results.len(),
                    candidate_limit,
                    distance_threshold,
                    "Completed Chronicle retrieval"
                );

                if results.is_empty() {
                    RetrievalOutcome::NoResultMeetsThreshold
                } else {
                    RetrievalOutcome::Results(results)
                }
            })
    }
}
