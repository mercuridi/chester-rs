use std::collections::HashMap;

use serde::Deserialize;

use super::chronicle::{
    default_llm_context_limit, default_pagerank_weight, default_retrieval_candidate_limit,
    default_retrieval_distance_threshold, default_retrieval_max_chunks_per_document,
    default_retrieval_near_duplicate_threshold, default_synthesis_batch_token_budget,
    default_synthesis_candidate_limit, default_synthesis_max_batches,
    default_synthesis_max_chunks_per_document, default_synthesis_retrieval_limit,
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawConfig {
    #[serde(default)]
    pub(crate) alias_groups: HashMap<String, RawAliasGroup>,
    #[serde(default)]
    pub(crate) guilds: HashMap<String, RawGuildConfig>,
    pub(crate) chronicle: RawChronicleConfig,
    pub(crate) database: RawDatabaseConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawDatabaseConfig {
    pub(crate) jester: String,
    pub(crate) chronicle: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawChronicleConfig {
    pub(crate) llm_repo: String,
    pub(crate) llm_revision: String,
    pub(crate) llm_model_file: String,
    pub(crate) llm_tokenizer_repo: String,
    pub(crate) llm_tokenizer_file: String,
    pub(crate) corpus_dir: String,
    pub(crate) llm_max_tokens: u32,
    #[serde(default = "default_llm_context_limit")]
    pub(crate) llm_context_limit: usize,
    pub(crate) llm_temperature: f32,
    pub(crate) llm_seed: u64,
    pub(crate) llm_system_prompt: String,
    pub(crate) llm_max_reply_length: usize,
    pub(crate) retrieval_limit: usize,
    #[serde(default = "default_retrieval_candidate_limit")]
    pub(crate) retrieval_candidate_limit: usize,
    #[serde(default = "default_retrieval_distance_threshold")]
    pub(crate) retrieval_distance_threshold: f32,
    #[serde(default = "default_retrieval_near_duplicate_threshold")]
    pub(crate) retrieval_near_duplicate_threshold: f32,
    #[serde(default = "default_retrieval_max_chunks_per_document")]
    pub(crate) retrieval_max_chunks_per_document: usize,
    #[serde(default = "default_pagerank_weight")]
    pub(crate) pagerank_weight: f64,
    #[serde(default = "default_synthesis_retrieval_limit")]
    pub(crate) synthesis_retrieval_limit: usize,
    #[serde(default = "default_synthesis_candidate_limit")]
    pub(crate) synthesis_candidate_limit: usize,
    #[serde(default = "default_synthesis_max_chunks_per_document")]
    pub(crate) synthesis_max_chunks_per_document: usize,
    #[serde(default = "default_synthesis_batch_token_budget")]
    pub(crate) synthesis_batch_token_budget: usize,
    #[serde(default = "default_synthesis_max_batches")]
    pub(crate) synthesis_max_batches: usize,
    pub(crate) max_chunk_tokens: usize,
    pub(crate) chunk_overlap_tokens: usize,
    #[serde(default)]
    pub(crate) gm_user_ids: Vec<String>,
    #[serde(default)]
    pub(crate) excluded_note_ids: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawAliasGroup {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) aliases: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawGuildConfig {
    #[serde(default)]
    pub(crate) alias_groups: Vec<String>,
}
