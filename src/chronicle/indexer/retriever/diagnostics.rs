use serde::Serialize;

use super::SearchSettings;

#[derive(Debug, Serialize)]
pub struct CandidateDiagnostic {
    pub document: String,
    pub chunk_index: i64,
    pub vector_rank: Option<usize>,
    pub vector_distance: Option<f32>,
    pub vector_passed_threshold: bool,
    pub lexical_rank: Option<usize>,
    pub fused_rank: Option<usize>,
    pub rrf_score: f64,
    pub pagerank_score: Option<f64>,
    pub pagerank_rank: Option<i64>,
    pub pagerank_contribution: f64,
    pub final_fusion_score: f64,
    pub decision: &'static str,
}

#[derive(Debug, Serialize)]
pub struct RetrievalDiagnostics {
    pub settings: SearchSettings,
    pub candidates: Vec<CandidateDiagnostic>,
}
