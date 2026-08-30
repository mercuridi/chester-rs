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

#[async_trait::async_trait]
pub trait RetrieverApi: Send + Sync {
    async fn search(
        &self,
        query: &str,
        limit: usize,
        candidate_limit: usize,
        distance_threshold: f32,
        near_duplicate_threshold: f32,
        max_chunks_per_document: usize,
    ) -> Result<RetrievalOutcome>;
    async fn load_embedder(&self) -> Result<()>;
    fn unload_embedder(&self) -> Result<()>;
}

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

#[async_trait::async_trait]
impl RetrieverApi for Retriever {
    async fn search(
        &self,
        query: &str,
        limit: usize,
        candidate_limit: usize,
        distance_threshold: f32,
        near_duplicate_threshold: f32,
        max_chunks_per_document: usize,
    ) -> Result<RetrievalOutcome> {
        self.search(
            query,
            limit,
            candidate_limit,
            distance_threshold,
            near_duplicate_threshold,
            max_chunks_per_document,
        )
        .await
    }
    async fn load_embedder(&self) -> Result<()> {
        self.load_embedder().await
    }
    fn unload_embedder(&self) -> Result<()> {
        self.unload_embedder()
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

        if is_near_duplicate(&candidate, &accepted, near_duplicate_threshold) {
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

fn is_near_duplicate(candidate: &SearchResult, accepted: &[SearchResult], threshold: f32) -> bool {
    let candidate_shingles = shingles(&candidate.text);
    if candidate_shingles.is_empty() {
        return false;
    }

    accepted.iter().any(|result| {
        if are_overlapping_neighbors(candidate, result) {
            return false;
        }

        let accepted_shingles = shingles(&result.text);
        let intersection = candidate_shingles.intersection(&accepted_shingles).count();
        let union = candidate_shingles.union(&accepted_shingles).count();

        #[allow(clippy::cast_precision_loss)]
        let overlap = intersection as f32 / union as f32;
        overlap >= threshold
    })
}

/// Keep intentionally overlapping neighbors available so retrieval can return context spanning a
/// chunk boundary; exact duplicates are still removed before this check. The overlap marker is
/// stored with the later chunk because it owns the repeated text.
fn are_overlapping_neighbors(left: &SearchResult, right: &SearchResult) -> bool {
    if left.document_path != right.document_path
        || left.chunk_index.abs_diff(right.chunk_index) != 1
    {
        return false;
    }

    if left.chunk_index > right.chunk_index {
        left.overlaps_previous
    } else {
        right.overlaps_previous
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn result(
        document_path: &str,
        chunk_index: i64,
        text: &str,
        overlaps_previous: bool,
    ) -> SearchResult {
        SearchResult {
            document_path: document_path.to_owned(),
            chunk_index,
            heading: None,
            text: text.to_owned(),
            overlaps_previous,
            distance: 0.0,
        }
    }

    #[test]
    fn preserves_similar_adjacent_chunks_from_the_same_document() {
        let candidates = vec![
            result("notes.md", 0, "alpha beta gamma delta", false),
            result("notes.md", 1, "beta gamma delta epsilon", true),
        ];

        let (accepted, exact_duplicates, near_duplicates, document_cap) =
            deduplicate_and_diversify(candidates, 2, 0.5, 2);

        assert_eq!(accepted.len(), 2);
        assert_eq!(exact_duplicates, 0);
        assert_eq!(near_duplicates, 0);
        assert_eq!(document_cap, 0);
    }

    #[test]
    fn preserves_overlapping_neighbors_regardless_of_search_order() {
        let candidates = vec![
            result("notes.md", 1, "beta gamma delta epsilon", true),
            result("notes.md", 0, "alpha beta gamma delta", false),
        ];

        let (accepted, exact_duplicates, near_duplicates, document_cap) =
            deduplicate_and_diversify(candidates, 2, 0.5, 2);

        assert_eq!(accepted.len(), 2);
        assert_eq!(exact_duplicates, 0);
        assert_eq!(near_duplicates, 0);
        assert_eq!(document_cap, 0);
    }

    #[test]
    fn filters_similar_chunks_without_a_persisted_overlap() {
        let candidates = vec![
            result("notes.md", 0, "alpha beta gamma delta", false),
            result("notes.md", 1, "beta gamma delta epsilon", false),
        ];

        let (accepted, exact_duplicates, near_duplicates, document_cap) =
            deduplicate_and_diversify(candidates, 2, 0.5, 2);

        assert_eq!(accepted.len(), 1);
        assert_eq!(exact_duplicates, 0);
        assert_eq!(near_duplicates, 1);
        assert_eq!(document_cap, 0);
    }

    #[test]
    fn canonical_text_collapses_whitespace_without_changing_case() {
        assert_eq!(canonical_text("  Alpha\n beta\t"), "Alpha beta");
        assert_ne!(canonical_text("Alpha"), canonical_text("alpha"));
    }

    #[test]
    fn shingles_are_case_insensitive_bigrams() {
        let actual = shingles("Alpha BETA gamma");
        assert_eq!(actual.len(), 2);
        assert!(actual.contains("alpha beta"));
        assert!(actual.contains("beta gamma"));
        assert!(shingles("single").is_empty());
        assert!(shingles("").is_empty());
    }

    #[test]
    fn removes_exact_duplicates_after_whitespace_normalisation() {
        let candidates = vec![
            result("a.md", 0, "alpha beta", false),
            result("b.md", 0, " alpha   beta ", false),
        ];
        let (accepted, exact, near, cap) = deduplicate_and_diversify(candidates, 10, 1.0, 10);
        assert_eq!(accepted.len(), 1);
        assert_eq!((exact, near, cap), (1, 0, 0));
    }

    #[test]
    fn exact_duplicate_detection_is_case_sensitive() {
        let candidates = vec![
            result("a.md", 0, "Alpha beta", false),
            result("b.md", 0, "alpha beta", false),
        ];
        let (accepted, exact, _, _) = deduplicate_and_diversify(candidates, 10, 1.1, 10);
        assert_eq!(accepted.len(), 2);
        assert_eq!(exact, 0);
    }

    #[test]
    fn applies_per_document_cap_and_preserves_ranked_order() {
        let candidates = vec![
            result("a.md", 0, "one alpha", false),
            result("a.md", 1, "two beta", false),
            result("b.md", 0, "three gamma", false),
        ];
        let (accepted, exact, near, cap) = deduplicate_and_diversify(candidates, 10, 1.1, 1);
        assert_eq!(accepted.len(), 2);
        assert_eq!(accepted[0].text, "one alpha");
        assert_eq!(accepted[1].text, "three gamma");
        assert_eq!((exact, near, cap), (0, 0, 1));
    }

    #[test]
    fn respects_zero_and_finite_result_limits() {
        let candidates = vec![
            result("a", 0, "one alpha", false),
            result("b", 0, "two beta", false),
        ];
        assert!(
            deduplicate_and_diversify(candidates.clone(), 0, 1.1, 10)
                .0
                .is_empty()
        );
        let accepted = deduplicate_and_diversify(candidates, 1, 1.1, 10).0;
        assert_eq!(accepted.len(), 1);
        assert_eq!(accepted[0].document_path, "a");
    }

    #[test]
    fn identifies_only_marked_adjacent_chunks_as_overlap_neighbors() {
        assert!(are_overlapping_neighbors(
            &result("a", 0, "a b", false),
            &result("a", 1, "b c", true)
        ));
        assert!(!are_overlapping_neighbors(
            &result("a", 0, "a b", false),
            &result("a", 1, "b c", false)
        ));
        assert!(!are_overlapping_neighbors(
            &result("a", 0, "a b", false),
            &result("a", 2, "b c", true)
        ));
        assert!(!are_overlapping_neighbors(
            &result("a", 0, "a b", false),
            &result("b", 1, "b c", true)
        ));
    }

    #[test]
    fn single_word_candidates_are_not_near_duplicates() {
        let accepted = vec![result("a", 0, "word", false)];
        assert!(!is_near_duplicate(
            &result("b", 0, "word", false),
            &accepted,
            0.0
        ));
    }
}
