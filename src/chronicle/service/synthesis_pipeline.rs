use std::collections::BTreeMap;

use super::super::{
    config::chronicle::SynthesisSettings,
    indexer::{
        db::repository::facade::{AccessScope, SearchResult},
        retriever::{
            CandidatePoolPolicy, FusionPolicy, RetrievalLimits, RetrievalOutcome, RetrieverApi,
            SearchSettings, SelectionPolicy,
        },
    },
};

#[derive(Debug, Clone, serde::Serialize)]
pub struct RetrievedDocumentDiagnostic {
    pub id: String,
    pub rank: usize,
    pub distance: f32,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SynthesisDiagnostics {
    pub retrieved_documents: Vec<RetrievedDocumentDiagnostic>,
    pub accepted_result_count: usize,
    pub omitted_result_count: usize,
    pub chunks_per_document: BTreeMap<String, usize>,
    pub map_batch_count: usize,
    pub reduction_pass_count: usize,
    pub prompt_token_counts: Vec<usize>,
    pub final_answer_length_retried: bool,
    pub final_answer_truncated: bool,
}

impl SynthesisDiagnostics {
    pub(in crate::chronicle::service) fn from_results(results: &[SearchResult]) -> Self {
        let mut chunks_per_document = BTreeMap::new();
        for result in results {
            *chunks_per_document
                .entry(result.document_path.clone())
                .or_insert(0) += 1;
        }
        Self {
            retrieved_documents: results
                .iter()
                .enumerate()
                .map(|(rank, result)| RetrievedDocumentDiagnostic {
                    id: std::path::Path::new(&result.document_path)
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                    rank: rank + 1,
                    distance: result.distance,
                })
                .collect(),
            accepted_result_count: results.len(),
            omitted_result_count: 0,
            chunks_per_document,
            map_batch_count: 0,
            reduction_pass_count: 0,
            prompt_token_counts: Vec::new(),
            final_answer_length_retried: false,
            final_answer_truncated: false,
        }
    }
}

pub(in crate::chronicle::service) enum EvidenceRetrieval {
    Evidence(Vec<SearchResult>),
    ImmediateResponse(&'static str),
}

pub(in crate::chronicle::service) async fn retrieve_evidence(
    retriever: &dyn RetrieverApi,
    synthesis: &SynthesisSettings,
    distance_threshold: f32,
    near_duplicate_threshold: f32,
    pagerank_weight: f64,
    question: &str,
    access: AccessScope,
) -> EvidenceRetrieval {
    let settings = SearchSettings {
        limits: RetrievalLimits {
            limit: synthesis.retrieval_limit,
            candidate_limit: synthesis.candidate_limit,
        },
        candidate_pool: CandidatePoolPolicy { distance_threshold },
        fusion: FusionPolicy {
            vector_rrf_weight: 1.0,
            lexical_rrf_weight: 1.0,
            pagerank_weight,
            rrf_rank_constant: 60.0,
        },
        selection: SelectionPolicy {
            near_duplicate_threshold,
            max_chunks_per_document: synthesis.max_chunks_per_document,
        },
    };
    match retriever.search(question, settings, access).await {
        Ok(RetrievalOutcome::Results(results)) => EvidenceRetrieval::Evidence(results),
        Ok(RetrievalOutcome::BadQuestion) => {
            EvidenceRetrieval::ImmediateResponse("Please provide a non-empty question.")
        }
        Ok(RetrievalOutcome::CorpusEmpty) => {
            EvidenceRetrieval::ImmediateResponse("Chronicle corpus is empty.")
        }
        Ok(RetrievalOutcome::NoResultMeetsThreshold) => {
            EvidenceRetrieval::ImmediateResponse("No relevant Chronicle context was found.")
        }
        Err(error) => {
            tracing::warn!(%error, "Chronicle synthesis retrieval failed");
            EvidenceRetrieval::ImmediateResponse("Chronicle retrieval failed.")
        }
    }
}
