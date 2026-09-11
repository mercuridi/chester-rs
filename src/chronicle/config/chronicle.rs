use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serenity::all::UserId;

use crate::chronicle::indexer::retriever::settings::{
    CandidatePoolPolicy, FusionPolicy, RetrievalLimits, SearchSettings, SelectionPolicy,
};
use crate::discord::constants::MESSAGE_MAX_CHARS;

use super::paths::resolve_path;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileChronicleConfig {
    indexing: FileIndexingSettings,
    llm: FileLlmSettings,
    retrieval: FileRetrievalSettings,
    synthesis: FileSynthesisSettings,
    access: FileChronicleAccessSettings,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileIndexingSettings {
    corpus_dir: String,
    max_chunk_tokens: usize,
    chunk_overlap_tokens: usize,
    excluded_note_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileLlmSettings {
    model: FileModelSource,
    tokenizer: FileTokenizerSource,
    generation: FileGenerationSettings,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileModelSource {
    repo: String,
    revision: String,
    file: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileTokenizerSource {
    repo: String,
    file: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileGenerationSettings {
    max_tokens: u32,
    context_limit: usize,
    temperature: f32,
    seed: u64,
    system_prompt: String,
    max_reply_length: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileRetrievalSettings {
    limit: usize,
    candidate_limit: usize,
    distance_threshold: f32,
    near_duplicate_threshold: f32,
    max_chunks_per_document: usize,
    pagerank_weight: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileSynthesisSettings {
    retrieval_limit: usize,
    candidate_limit: usize,
    max_chunks_per_document: usize,
    batch_token_budget: usize,
    max_batches: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileChronicleAccessSettings {
    gm_user_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ChronicleConfig {
    pub llm: LlmSettings,
    pub retrieval: RetrievalSettings,
    pub indexing: IndexingSettings,
    pub synthesis: SynthesisSettings,
    pub access: ChronicleAccessSettings,
}

#[derive(Debug, Clone)]
pub struct LlmSettings {
    pub model: ModelSource,
    pub tokenizer: TokenizerSource,
    pub generation: GenerationSettings,
}

#[derive(Debug, Clone)]
pub struct ModelSource {
    pub repo: String,
    pub revision: String,
    pub file: String,
}

#[derive(Debug, Clone)]
pub struct TokenizerSource {
    pub repo: String,
    pub file: String,
}

#[derive(Debug, Clone)]
pub struct GenerationSettings {
    pub max_tokens: u32,
    pub context_limit: usize,
    pub temperature: f32,
    pub seed: u64,
    pub system_prompt: String,
    pub max_reply_length: usize,
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
    pub corpus_dir: PathBuf,
    pub max_chunk_tokens: usize,
    pub chunk_overlap_tokens: usize,
    pub excluded_note_ids: HashSet<String>,
}

#[derive(Debug, Clone)]
pub struct ChronicleAccessSettings {
    gm_user_ids: HashSet<UserId>,
}

impl ChronicleAccessSettings {
    pub fn is_gm(&self, user_id: UserId) -> bool {
        self.gm_user_ids.contains(&user_id)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SynthesisSettings {
    pub retrieval_limit: usize,
    pub candidate_limit: usize,
    pub max_chunks_per_document: usize,
    pub batch_token_budget: usize,
    pub max_batches: usize,
}

impl ChronicleConfig {
    pub(crate) fn from_file(file: FileChronicleConfig, project_root: &Path) -> Result<Self> {
        let config = Self {
            llm: LlmSettings {
                model: ModelSource {
                    repo: file.llm.model.repo,
                    revision: file.llm.model.revision,
                    file: file.llm.model.file,
                },
                tokenizer: TokenizerSource {
                    repo: file.llm.tokenizer.repo,
                    file: file.llm.tokenizer.file,
                },
                generation: GenerationSettings {
                    max_tokens: file.llm.generation.max_tokens,
                    context_limit: file.llm.generation.context_limit,
                    temperature: file.llm.generation.temperature,
                    seed: file.llm.generation.seed,
                    system_prompt: file.llm.generation.system_prompt,
                    max_reply_length: file.llm.generation.max_reply_length,
                },
            },
            retrieval: RetrievalSettings {
                limit: file.retrieval.limit,
                candidate_limit: file.retrieval.candidate_limit,
                distance_threshold: file.retrieval.distance_threshold,
                near_duplicate_threshold: file.retrieval.near_duplicate_threshold,
                max_chunks_per_document: file.retrieval.max_chunks_per_document,
                pagerank_weight: file.retrieval.pagerank_weight,
            },
            indexing: IndexingSettings {
                corpus_dir: resolve_path(project_root, &file.indexing.corpus_dir),
                max_chunk_tokens: file.indexing.max_chunk_tokens,
                chunk_overlap_tokens: file.indexing.chunk_overlap_tokens,
                excluded_note_ids: parse_excluded_note_ids(file.indexing.excluded_note_ids)?,
            },
            synthesis: SynthesisSettings {
                retrieval_limit: file.synthesis.retrieval_limit,
                candidate_limit: file.synthesis.candidate_limit,
                max_chunks_per_document: file.synthesis.max_chunks_per_document,
                batch_token_budget: file.synthesis.batch_token_budget,
                max_batches: file.synthesis.max_batches,
            },
            access: ChronicleAccessSettings {
                gm_user_ids: parse_gm_user_ids(file.access.gm_user_ids)?,
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
        let max_tokens = usize::try_from(self.llm.generation.max_tokens)
            .context("Chronicle llm_max_tokens does not fit in usize")?;
        let prompt_token_budget = self.llm.generation.context_limit - max_tokens;
        if self.synthesis.batch_token_budget > prompt_token_budget {
            bail!(
                "Chronicle synthesis_batch_token_budget must be between 1 and the available LLM prompt token budget ({prompt_token_budget})"
            );
        }
        Ok(())
    }
}

impl GenerationSettings {
    fn validate(&self) -> Result<()> {
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
        self.generation.validate()
    }
}

impl RetrievalSettings {
    pub fn search_settings(&self) -> SearchSettings {
        SearchSettings {
            limits: RetrievalLimits {
                limit: self.limit,
                candidate_limit: self.candidate_limit,
            },
            candidate_pool: CandidatePoolPolicy {
                distance_threshold: self.distance_threshold,
            },
            fusion: FusionPolicy {
                vector_rrf_weight: 1.0,
                lexical_rrf_weight: 1.0,
                pagerank_weight: self.pagerank_weight,
                rrf_rank_constant: 60.0,
            },
            selection: SelectionPolicy {
                near_duplicate_threshold: self.near_duplicate_threshold,
                max_chunks_per_document: self.max_chunks_per_document,
            },
        }
    }

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
    pub fn search_settings(&self, retrieval: &RetrievalSettings) -> SearchSettings {
        SearchSettings {
            limits: RetrievalLimits {
                limit: self.retrieval_limit,
                candidate_limit: self.candidate_limit,
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
                max_chunks_per_document: self.max_chunks_per_document,
            },
        }
    }

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

fn parse_gm_user_ids(raw_user_ids: Vec<String>) -> Result<HashSet<UserId>> {
    let mut user_ids = HashSet::new();
    for raw_user_id in raw_user_ids {
        let id = raw_user_id
            .parse::<u64>()
            .with_context(|| format!("Invalid Chronicle GM user ID `{raw_user_id}`"))?;
        if id == 0 {
            bail!("Discord user ID cannot be zero");
        }
        if !user_ids.insert(UserId::new(id)) {
            bail!("Duplicate Chronicle GM user ID `{raw_user_id}`");
        }
    }
    Ok(user_ids)
}
