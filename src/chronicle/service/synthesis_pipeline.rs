use std::collections::BTreeMap;

use crate::chronicle::indexer::{
    AccessScope, RetrievalOutcome, RetrieverApi, SearchResult, from_synthesis_config,
};
use crate::config::{RetrievalSettings, SynthesisSettings};

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
    retrieval: &RetrievalSettings,
    synthesis: &SynthesisSettings,
    question: &str,
    access: AccessScope,
) -> EvidenceRetrieval {
    let settings = from_synthesis_config(synthesis, retrieval);
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
