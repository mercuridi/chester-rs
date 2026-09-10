use std::{collections::HashSet, path::Path};

use anyhow::{Context, Result, bail};

use crate::discord::constants::MESSAGE_MAX_CHARS;

use super::{paths::resolve_path, raw::RawChronicleConfig};

#[derive(Debug, Clone)]
pub struct ChronicleConfig {
    pub llm: LlmSettings,
    pub retrieval: RetrievalSettings,
    pub indexing: IndexingSettings,
    pub synthesis: SynthesisSettings,
}

#[derive(Debug, Clone)]
pub struct LlmSettings {
    pub model: RepositoryFile,
    pub tokenizer: RepositoryFile,
    pub max_tokens: u32,
    pub context_limit: usize,
    pub temperature: f32,
    pub seed: u64,
    pub system_prompt: String,
    pub max_reply_length: usize,
}

#[derive(Debug, Clone)]
pub struct RepositoryFile {
    pub repo: String,
    pub revision: String,
    pub file: String,
}

#[derive(Debug, Clone, Copy)]
pub struct RetrievalSettings {
    pub limit: usize,
    pub candidate_limit: usize,
    pub distance_threshold: f32,
    pub near_duplicate_threshold: f32,
    pub max_chunks_per_document: usize,
    pub pagerank_weight: f64,
}

#[derive(Debug, Clone)]
pub struct IndexingSettings {
    pub corpus_dir: std::path::PathBuf,
    pub max_chunk_tokens: usize,
    pub chunk_overlap_tokens: usize,
    pub excluded_note_ids: HashSet<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct SynthesisSettings {
    pub retrieval_limit: usize,
    pub candidate_limit: usize,
    pub max_chunks_per_document: usize,
    pub batch_token_budget: usize,
    pub max_batches: usize,
}

impl Default for SynthesisSettings {
    fn default() -> Self {
        Self {
            retrieval_limit: default_synthesis_retrieval_limit(),
            candidate_limit: default_synthesis_candidate_limit(),
            max_chunks_per_document: default_synthesis_max_chunks_per_document(),
            batch_token_budget: default_synthesis_batch_token_budget(),
            max_batches: default_synthesis_max_batches(),
        }
    }
}

impl ChronicleConfig {
    pub(crate) fn from_raw(raw: RawChronicleConfig, project_root: &Path) -> Result<Self> {
        let config = Self {
            llm: LlmSettings {
                model: RepositoryFile {
                    repo: raw.llm_repo,
                    revision: raw.llm_revision,
                    file: raw.llm_model_file,
                },
                tokenizer: RepositoryFile {
                    repo: raw.llm_tokenizer_repo,
                    revision: String::new(),
                    file: raw.llm_tokenizer_file,
                },
                max_tokens: raw.llm_max_tokens,
                context_limit: raw.llm_context_limit,
                temperature: raw.llm_temperature,
                seed: raw.llm_seed,
                system_prompt: raw.llm_system_prompt,
                max_reply_length: raw.llm_max_reply_length,
            },
            retrieval: RetrievalSettings {
                limit: raw.retrieval_limit,
                candidate_limit: raw.retrieval_candidate_limit,
                distance_threshold: raw.retrieval_distance_threshold,
                near_duplicate_threshold: raw.retrieval_near_duplicate_threshold,
                max_chunks_per_document: raw.retrieval_max_chunks_per_document,
                pagerank_weight: raw.pagerank_weight,
            },
            indexing: IndexingSettings {
                corpus_dir: resolve_path(project_root, &raw.corpus_dir),
                max_chunk_tokens: raw.max_chunk_tokens,
                chunk_overlap_tokens: raw.chunk_overlap_tokens,
                excluded_note_ids: parse_excluded_note_ids(raw.excluded_note_ids)?,
            },
            synthesis: SynthesisSettings {
                retrieval_limit: raw.synthesis_retrieval_limit,
                candidate_limit: raw.synthesis_candidate_limit,
                max_chunks_per_document: raw.synthesis_max_chunks_per_document,
                batch_token_budget: raw.synthesis_batch_token_budget,
                max_batches: raw.synthesis_max_batches,
            },
        };
        config.validate()?;
        Ok(config)
    }

    pub(crate) fn validate(&self) -> Result<()> {
        self.llm.validate()?;
        self.retrieval.validate()?;
        self.indexing.validate()?;
        self.synthesis.validate()?;
        let max_tokens = usize::try_from(self.llm.max_tokens)
            .context("Chronicle llm_max_tokens does not fit in usize")?;
        let prompt_token_budget = self.llm.context_limit - max_tokens;
        if self.synthesis.batch_token_budget > prompt_token_budget {
            bail!(
                "Chronicle synthesis_batch_token_budget must be between 1 and the available LLM prompt token budget ({prompt_token_budget})"
            );
        }
        Ok(())
    }
}

impl LlmSettings {
    fn validate(&self) -> Result<()> {
        if self.model.repo.trim().is_empty()
            || self.model.revision.trim().is_empty()
            || self.model.file.trim().is_empty()
            || self.tokenizer.repo.trim().is_empty()
            || self.tokenizer.file.trim().is_empty()
        {
            bail!("Chronicle LLM repository and file settings cannot be empty");
        }
        if self.system_prompt.trim().is_empty() {
            bail!("Chronicle llm_system_prompt cannot be empty");
        }
        if self.max_tokens == 0 || self.max_tokens > 32_768 {
            bail!("Chronicle llm_max_tokens must be between 1 and 32768");
        }
        let max_tokens = usize::try_from(self.max_tokens)
            .context("Chronicle llm_max_tokens does not fit in usize")?;
        if self.context_limit <= max_tokens || self.context_limit > 32_768 {
            bail!(
                "Chronicle llm_context_limit must be greater than llm_max_tokens and no greater than 32768"
            );
        }
        if self.max_reply_length == 0 || self.max_reply_length > MESSAGE_MAX_CHARS {
            bail!("Chronicle llm_max_reply_length must be between 1 and {MESSAGE_MAX_CHARS}");
        }
        if !self.temperature.is_finite() || !(0.0..=2.0).contains(&self.temperature) {
            bail!("Chronicle llm_temperature must be finite and between 0.0 and 2.0");
        }
        Ok(())
    }
}

impl RetrievalSettings {
    fn validate(&self) -> Result<()> {
        if self.limit == 0 || self.limit > 100 {
            bail!("Chronicle retrieval_limit must be between 1 and 100");
        }
        if self.candidate_limit < self.limit || self.candidate_limit > 1000 {
            bail!("Chronicle retrieval_candidate_limit must be between retrieval_limit and 1000");
        }
        if !self.distance_threshold.is_finite() || self.distance_threshold < 0.0 {
            bail!("Chronicle retrieval_distance_threshold must be finite and non-negative");
        }
        if !self.near_duplicate_threshold.is_finite()
            || !(0.0..=1.0).contains(&self.near_duplicate_threshold)
        {
            bail!(
                "Chronicle retrieval_near_duplicate_threshold must be finite and between 0.0 and 1.0"
            );
        }
        if self.max_chunks_per_document == 0 {
            bail!("Chronicle retrieval_max_chunks_per_document must be greater than zero");
        }
        if !self.pagerank_weight.is_finite() || !(0.0..=1.0).contains(&self.pagerank_weight) {
            bail!("Chronicle pagerank_weight must be finite and between 0.0 and 1.0");
        }
        Ok(())
    }
}

impl IndexingSettings {
    fn validate(&self) -> Result<()> {
        if self.corpus_dir.as_os_str().is_empty() {
            bail!("Chronicle corpus_dir cannot be empty");
        }
        if !(3..=512).contains(&self.max_chunk_tokens) {
            bail!("Chronicle max_chunk_tokens must be between 3 and 512");
        }
        if self.chunk_overlap_tokens > self.max_chunk_tokens.saturating_sub(3) {
            bail!(
                "Chronicle chunk_overlap_tokens must be no greater than max_chunk_tokens minus 3"
            );
        }
        Ok(())
    }
}

impl SynthesisSettings {
    fn validate(&self) -> Result<()> {
        if self.retrieval_limit == 0 || self.retrieval_limit > 100 {
            bail!("Chronicle synthesis_retrieval_limit must be between 1 and 100");
        }
        if self.candidate_limit < self.retrieval_limit || self.candidate_limit > 1000 {
            bail!(
                "Chronicle synthesis_candidate_limit must be between synthesis_retrieval_limit and 1000"
            );
        }
        if self.max_chunks_per_document == 0 {
            bail!("Chronicle synthesis_max_chunks_per_document must be greater than zero");
        }
        if self.batch_token_budget == 0 {
            bail!(
                "Chronicle synthesis_batch_token_budget must be between 1 and the available LLM prompt token budget"
            );
        }
        if self.max_batches == 0 || self.max_batches > 100 {
            bail!("Chronicle synthesis_max_batches must be between 1 and 100");
        }
        Ok(())
    }
}

fn parse_excluded_note_ids(raw_note_ids: Vec<String>) -> Result<HashSet<String>> {
    let mut note_ids = HashSet::new();
    for note_id in raw_note_ids {
        if note_id.trim().is_empty() {
            bail!("Chronicle excluded note IDs cannot be empty");
        }
        if !note_ids.insert(note_id.clone()) {
            bail!("Duplicate Chronicle excluded note ID `{note_id}`");
        }
    }
    Ok(note_ids)
}

pub(crate) fn default_llm_context_limit() -> usize {
    8_192
}
pub(crate) fn default_retrieval_candidate_limit() -> usize {
    15
}
pub(crate) fn default_retrieval_distance_threshold() -> f32 {
    0.8
}
pub(crate) fn default_retrieval_near_duplicate_threshold() -> f32 {
    0.85
}
pub(crate) fn default_retrieval_max_chunks_per_document() -> usize {
    2
}
pub(crate) fn default_pagerank_weight() -> f64 {
    0.15
}
pub(crate) fn default_synthesis_retrieval_limit() -> usize {
    12
}
pub(crate) fn default_synthesis_candidate_limit() -> usize {
    40
}
pub(crate) fn default_synthesis_max_chunks_per_document() -> usize {
    3
}
pub(crate) fn default_synthesis_batch_token_budget() -> usize {
    1_800
}
pub(crate) fn default_synthesis_max_batches() -> usize {
    6
}
