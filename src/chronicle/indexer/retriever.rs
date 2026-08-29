use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
};

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

    #[instrument(skip(self, query), fields(query_len = query.len(), limit, candidate_limit, distance_threshold, near_duplicate_threshold, max_chunks_per_document))]
    pub async fn search(
        &self,
        query: &str,
        limit: usize,
        candidate_limit: usize,
        distance_threshold: f32,
        near_duplicate_threshold: f32,
        max_chunks_per_document: usize,
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
                let threshold_results = results
                    .into_iter()
                    .filter(|result| result.distance <= distance_threshold)
                    .collect::<Vec<_>>();
                let threshold_result_count = threshold_results.len();
                let (results, exact_duplicates, near_duplicates, document_cap) =
                    deduplicate_and_diversify(
                        threshold_results,
                        limit,
                        near_duplicate_threshold,
                        max_chunks_per_document,
                    );

                debug!(
                    result_count = results.len(),
                    threshold_result_count,
                    exact_duplicates,
                    near_duplicates,
                    document_cap,
                    distinct_documents = results
                        .iter()
                        .map(|result| result.document_path.as_str())
                        .collect::<HashSet<_>>()
                        .len(),
                    candidate_limit,
                    distance_threshold,
                    near_duplicate_threshold,
                    max_chunks_per_document,
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

fn deduplicate_and_diversify(
    candidates: Vec<SearchResult>,
    limit: usize,
    near_duplicate_threshold: f32,
    max_chunks_per_document: usize,
) -> (Vec<SearchResult>, usize, usize, usize) {
    let mut accepted = Vec::with_capacity(limit);
    let mut exact_keys = HashSet::new();
    let mut document_counts = HashMap::new();
    let mut exact_duplicates = 0;
    let mut near_duplicates = 0;
    let mut document_cap = 0;

    for candidate in candidates {
        if accepted.len() >= limit {
            break;
        }

        let exact_key = canonical_text(&candidate.text);
        if !exact_keys.insert(exact_key) {
            exact_duplicates += 1;
            continue;
        }

        if is_near_duplicate(&candidate.text, &accepted, near_duplicate_threshold) {
            near_duplicates += 1;
            continue;
        }

        let document_count = document_counts
            .entry(candidate.document_path.clone())
            .or_insert(0);
        if *document_count >= max_chunks_per_document {
            document_cap += 1;
            continue;
        }

        *document_count += 1;
        accepted.push(candidate);
    }

    (accepted, exact_duplicates, near_duplicates, document_cap)
}

fn canonical_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_near_duplicate(candidate: &str, accepted: &[SearchResult], threshold: f32) -> bool {
    let candidate_shingles = shingles(candidate);
    if candidate_shingles.is_empty() {
        return false;
    }

    accepted.iter().any(|result| {
        let accepted_shingles = shingles(&result.text);
        let intersection = candidate_shingles.intersection(&accepted_shingles).count();
        let union = candidate_shingles.union(&accepted_shingles).count();

        #[allow(clippy::cast_precision_loss)]
        let overlap = intersection as f32 / union as f32;
        overlap >= threshold
    })
}

fn shingles(text: &str) -> HashSet<String> {
    let words = text
        .split_whitespace()
        .map(str::to_lowercase)
        .collect::<Vec<_>>();

    if words.len() < 2 {
        return HashSet::new();
    }

    words
        .windows(2)
        .map(|window| format!("{} {}", window[0], window[1]))
        .collect()
}
