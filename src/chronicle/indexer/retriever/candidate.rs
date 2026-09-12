use super::diagnostics::CandidateDiagnostic;
use crate::chronicle::indexer::db::{PageRankSignal, SearchResult};

pub(super) type CandidateKey = (String, i64);

#[derive(Debug)]
pub(super) struct RankedCandidate {
    pub(super) result: SearchResult,
    pub(super) vector_rank: Option<usize>,
    pub(super) vector_distance: Option<f32>,
    pub(super) vector_passed_threshold: bool,
    pub(super) lexical_rank: Option<usize>,
    pub(super) rrf_score: f64,
    pub(super) pagerank_score: Option<f64>,
    pub(super) pagerank_rank: Option<i64>,
    pub(super) pagerank_contribution: f64,
    pub(super) reranker_contribution: f64,
    pub(super) final_fusion_score: f64,
    pub(super) eligible: bool,
    pub(super) final_rank: Option<usize>,
    pub(super) decision: &'static str,
}

impl RankedCandidate {
    pub(super) fn new(result: SearchResult, pagerank: Option<PageRankSignal>) -> Self {
        Self {
            result,
            vector_rank: None,
            vector_distance: None,
            vector_passed_threshold: false,
            lexical_rank: None,
            rrf_score: 0.0,
            pagerank_score: pagerank.map(|signal| signal.score),
            pagerank_rank: pagerank.map(|signal| signal.rank),
            pagerank_contribution: 0.0,
            reranker_contribution: 0.0,
            final_fusion_score: 0.0,
            eligible: false,
            final_rank: None,
            decision: "vector_threshold",
        }
    }

    pub(super) fn key(&self) -> CandidateKey {
        (self.result.document_path.clone(), self.result.chunk_index)
    }

    pub(super) fn diagnostic(self) -> CandidateDiagnostic {
        CandidateDiagnostic {
            document: self.result.document_path,
            chunk_index: self.result.chunk_index,
            vector_rank: self.vector_rank,
            vector_distance: self.vector_distance,
            vector_passed_threshold: self.vector_passed_threshold,
            lexical_rank: self.lexical_rank,
            fused_rank: self.final_rank,
            rrf_score: self.rrf_score,
            pagerank_score: self.pagerank_score,
            pagerank_rank: self.pagerank_rank,
            pagerank_contribution: self.pagerank_contribution,
            final_fusion_score: self.final_fusion_score,
            decision: self.decision,
        }
    }
}
