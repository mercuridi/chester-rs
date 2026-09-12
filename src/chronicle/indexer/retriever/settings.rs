use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::config::{
    RetrievalSettings as ConfigRetrievalSettings, SynthesisSettings as ConfigSynthesisSettings,
};

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SearchSettings {
    pub limits: RetrievalLimits,
    pub candidate_pool: CandidatePoolPolicy,
    pub fusion: FusionPolicy,
    pub selection: SelectionPolicy,
}

/// Operational limits: candidate retrieval work and final context size.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RetrievalLimits {
    pub limit: usize,
    pub candidate_limit: usize,
}

/// Candidate-pool policy: determines which raw retrieval results reach fusion.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CandidatePoolPolicy {
    pub distance_threshold: f32,
}

/// Ranking policy: determines how eligible candidates are fused before reranking and selection.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FusionPolicy {
    pub vector_rrf_weight: f64,
    pub lexical_rrf_weight: f64,
    pub pagerank_weight: f64,
    pub rrf_rank_constant: f64,
}

/// Context-selection policy: applies after fusion (and a future reranking stage).
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SelectionPolicy {
    pub near_duplicate_threshold: f32,
    pub max_chunks_per_document: usize,
}

impl SearchSettings {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.limits.limit > 0
                && self.limits.candidate_limit >= self.limits.limit
                && self.limits.candidate_limit <= 1000,
            "Invalid retrieval limits"
        );
        anyhow::ensure!(
            self.candidate_pool.distance_threshold.is_finite()
                && self.candidate_pool.distance_threshold >= 0.0,
            "Invalid distance threshold"
        );
        anyhow::ensure!(
            (0.0..=1.0).contains(&self.selection.near_duplicate_threshold),
            "Invalid duplicate threshold"
        );
        anyhow::ensure!(
            self.selection.max_chunks_per_document > 0,
            "Document cap must be positive"
        );
        anyhow::ensure!(
            self.fusion.vector_rrf_weight.is_finite()
                && self.fusion.vector_rrf_weight >= 0.0
                && self.fusion.lexical_rrf_weight.is_finite()
                && self.fusion.lexical_rrf_weight >= 0.0
                && self.fusion.pagerank_weight.is_finite()
                && (0.0..=1.0).contains(&self.fusion.pagerank_weight)
                && self.fusion.rrf_rank_constant.is_finite()
                && self.fusion.rrf_rank_constant >= 0.0,
            "Invalid PageRank weight"
        );
        Ok(())
    }
}

pub(crate) fn from_retrieval_config(config: &ConfigRetrievalSettings) -> SearchSettings {
    SearchSettings {
        limits: RetrievalLimits {
            limit: config.limit,
            candidate_limit: config.candidate_limit,
        },
        candidate_pool: CandidatePoolPolicy {
            distance_threshold: config.distance_threshold,
        },
        fusion: FusionPolicy {
            vector_rrf_weight: 1.0,
            lexical_rrf_weight: 1.0,
            pagerank_weight: config.pagerank_weight,
            rrf_rank_constant: 60.0,
        },
        selection: SelectionPolicy {
            near_duplicate_threshold: config.near_duplicate_threshold,
            max_chunks_per_document: config.max_chunks_per_document,
        },
    }
}

pub(crate) fn from_synthesis_config(
    config: &ConfigSynthesisSettings,
    retrieval: &ConfigRetrievalSettings,
) -> SearchSettings {
    SearchSettings {
        limits: RetrievalLimits {
            limit: config.retrieval_limit,
            candidate_limit: config.candidate_limit,
        },
        candidate_pool: CandidatePoolPolicy {
            distance_threshold: retrieval.distance_threshold,
        },
        fusion: FusionPolicy {
            vector_rrf_weight: 1.0,
            lexical_rrf_weight: 1.0,
            pagerank_weight: retrieval.pagerank_weight,
            rrf_rank_constant: 60.0,
        },
        selection: SelectionPolicy {
            near_duplicate_threshold: retrieval.near_duplicate_threshold,
            max_chunks_per_document: config.max_chunks_per_document,
        },
    }
}
