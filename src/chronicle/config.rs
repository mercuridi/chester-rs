use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serenity::all::{GuildId, UserId};

use crate::discord::constants::MESSAGE_MAX_CHARS;

pub type AliasGroupId = String;

#[derive(Debug, Deserialize)]
struct RawConfig {
    #[serde(default)]
    alias_groups: HashMap<String, RawAliasGroup>,

    #[serde(default)]
    guilds: HashMap<String, RawGuildConfig>,

    chronicle: RawChronicleConfig,

    database: RawDatabaseConfig,
}

#[derive(Debug, Deserialize)]
struct RawDatabaseConfig {
    jester: String,

    chronicle: String,
}

#[derive(Debug, Deserialize)]
pub struct RawChronicleConfig {
    llm_repo: String,

    llm_revision: String,

    llm_model_file: String,

    llm_tokenizer_repo: String,

    llm_tokenizer_file: String,

    corpus_dir: String,

    llm_max_tokens: u32,

    #[serde(default = "default_llm_context_limit")]
    llm_context_limit: usize,

    llm_temperature: f32,

    llm_seed: u64,

    llm_system_prompt: String,

    llm_max_reply_length: usize,

    retrieval_limit: usize,

    #[serde(default = "default_retrieval_candidate_limit")]
    retrieval_candidate_limit: usize,

    #[serde(default = "default_retrieval_distance_threshold")]
    retrieval_distance_threshold: f32,

    #[serde(default = "default_retrieval_near_duplicate_threshold")]
    retrieval_near_duplicate_threshold: f32,

    #[serde(default = "default_retrieval_max_chunks_per_document")]
    retrieval_max_chunks_per_document: usize,

    max_chunk_tokens: usize,
}

#[derive(Debug, Deserialize, Clone)]
struct RawAliasGroup {
    name: String,

    #[serde(default)]
    aliases: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct RawGuildConfig {
    #[serde(default)]
    alias_groups: Vec<String>,
}

#[derive(Debug)]
pub struct Config {
    alias_groups: HashMap<AliasGroupId, AliasGroup>,
    guilds: HashMap<GuildId, GuildConfig>,
    pub database: DatabaseConfig,
    pub chronicle: ChronicleConfig,
    pub paths: AppPaths,
}

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub recordings_dir: PathBuf,
    pub audio_dir: PathBuf,
}

#[derive(Debug)]
pub struct DatabaseConfig {
    pub jester: String,
    pub chronicle: String,
}

#[derive(Debug, Clone)]
pub struct ChronicleConfig {
    pub llm_repo: String,
    pub llm_revision: String,
    pub llm_model_file: String,
    pub llm_tokenizer_repo: String,
    pub llm_tokenizer_file: String,
    pub corpus_dir: String,
    pub llm_max_tokens: u32,
    pub llm_context_limit: usize,
    pub llm_temperature: f32,
    pub llm_seed: u64,
    pub llm_system_prompt: String,
    pub llm_max_reply_length: usize,
    pub retrieval_limit: usize,
    pub retrieval_candidate_limit: usize,
    pub retrieval_distance_threshold: f32,
    pub retrieval_near_duplicate_threshold: f32,
    pub retrieval_max_chunks_per_document: usize,
    pub max_chunk_tokens: usize,
}

#[derive(Debug)]
pub struct AliasGroup {
    pub name: String,
    pub aliases: HashMap<UserId, String>,
}

#[derive(Debug)]
pub struct GuildConfig {
    pub alias_groups: Vec<AliasGroupId>,
}

fn default_retrieval_candidate_limit() -> usize {
    15
}

fn default_llm_context_limit() -> usize {
    32_768
}

fn default_retrieval_distance_threshold() -> f32 {
    0.8
}

fn default_retrieval_near_duplicate_threshold() -> f32 {
    0.85
}

fn default_retrieval_max_chunks_per_document() -> usize {
    2
}

fn resolve_path(project_root: &Path, path: &str) -> String {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_string_lossy().into_owned()
    } else {
        project_root.join(path).to_string_lossy().into_owned()
    }
}

fn resolve_sqlite_url(project_root: &Path, url: &str) -> String {
    let Some(path_and_query) = url.strip_prefix("sqlite://") else {
        return url.to_owned();
    };

    let (path, query) = path_and_query
        .split_once('?')
        .map_or((path_and_query, None), |(path, query)| (path, Some(query)));

    if path == ":memory:" || path.starts_with('/') {
        return url.to_owned();
    }

    let resolved = resolve_path(project_root, path);
    match query {
        Some(query) => format!("sqlite://{resolved}?{query}"),
        None => format!("sqlite://{resolved}"),
    }
}

impl Config {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();

        let contents = fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file {}", path.display()))?;

        let raw: RawConfig = toml::from_str(&contents)
            .with_context(|| format!("Failed to parse config file {}", path.display()))?;

        let project_root = path
            .parent()
            .and_then(Path::parent)
            .unwrap_or_else(|| Path::new("."));

        Self::from_raw(raw, project_root)
    }

    fn from_raw(raw: RawConfig, project_root: &Path) -> Result<Self> {
        let mut alias_groups = HashMap::new();

        for (group_id, raw_group) in raw.alias_groups {
            if group_id.trim().is_empty() {
                bail!("Alias group ID cannot be empty");
            }

            if raw_group.name.trim().is_empty() {
                bail!("Alias group `{group_id}` has an empty name");
            }

            let mut aliases = HashMap::new();

            for (raw_user_id, alias) in raw_group.aliases {
                let user_id = parse_user_id(&raw_user_id).with_context(|| {
                    format!("Invalid user ID `{raw_user_id}` in alias group `{group_id}`")
                })?;

                if alias.trim().is_empty() {
                    bail!(
                        "Alias for user `{raw_user_id}` in alias group `{group_id}` \
                        cannot be empty"
                    );
                }

                aliases.insert(user_id, alias);
            }

            alias_groups.insert(
                group_id,
                AliasGroup {
                    name: raw_group.name,
                    aliases,
                },
            );
        }

        let mut guilds = HashMap::new();

        for (raw_guild_id, raw_guild) in raw.guilds {
            let guild_id = parse_guild_id(&raw_guild_id)
                .with_context(|| format!("Invalid guild ID `{raw_guild_id}`"))?;

            for group_id in &raw_guild.alias_groups {
                if !alias_groups.contains_key(group_id) {
                    bail!("Guild `{raw_guild_id}` references unknown alias group `{group_id}`");
                }
            }

            guilds.insert(
                guild_id,
                GuildConfig {
                    alias_groups: raw_guild.alias_groups,
                },
            );
        }

        let chronicle = ChronicleConfig {
            llm_repo: raw.chronicle.llm_repo,
            llm_revision: raw.chronicle.llm_revision,
            llm_model_file: raw.chronicle.llm_model_file,
            llm_tokenizer_repo: raw.chronicle.llm_tokenizer_repo,
            llm_tokenizer_file: raw.chronicle.llm_tokenizer_file,
            corpus_dir: resolve_path(project_root, &raw.chronicle.corpus_dir),
            llm_max_tokens: raw.chronicle.llm_max_tokens,
            llm_context_limit: raw.chronicle.llm_context_limit,
            llm_temperature: raw.chronicle.llm_temperature,
            llm_seed: raw.chronicle.llm_seed,
            llm_system_prompt: raw.chronicle.llm_system_prompt,
            llm_max_reply_length: raw.chronicle.llm_max_reply_length,
            retrieval_limit: raw.chronicle.retrieval_limit,
            retrieval_candidate_limit: raw.chronicle.retrieval_candidate_limit,
            retrieval_distance_threshold: raw.chronicle.retrieval_distance_threshold,
            retrieval_near_duplicate_threshold: raw.chronicle.retrieval_near_duplicate_threshold,
            retrieval_max_chunks_per_document: raw.chronicle.retrieval_max_chunks_per_document,
            max_chunk_tokens: raw.chronicle.max_chunk_tokens,
        };

        chronicle.validate()?;

        if raw.database.jester.trim().is_empty() || raw.database.chronicle.trim().is_empty() {
            bail!("Database URLs cannot be empty");
        }

        Ok(Self {
            alias_groups,
            guilds,
            database: DatabaseConfig {
                jester: resolve_sqlite_url(project_root, &raw.database.jester),
                chronicle: resolve_sqlite_url(project_root, &raw.database.chronicle),
            },
            chronicle,
            paths: AppPaths {
                recordings_dir: project_root.join(".chronicle/recordings"),
                audio_dir: project_root.join("audio"),
            },
        })
    }

    pub fn alias_group(&self, group_id: &str) -> Option<&AliasGroup> {
        self.alias_groups.get(group_id)
    }

    pub fn alias_groups_for_guild(&self, guild_id: GuildId) -> Option<Vec<(&str, &AliasGroup)>> {
        let guild = self.guilds.get(&guild_id)?;

        Some(
            guild
                .alias_groups
                .iter()
                .filter_map(|group_id| {
                    self.alias_groups
                        .get(group_id)
                        .map(|group| (group_id.as_str(), group))
                })
                .collect(),
        )
    }

    pub fn validate_participants<'a>(
        &self,
        group_id: &str,
        participants: impl IntoIterator<Item = &'a UserId>,
    ) -> Result<(), AliasValidationError> {
        let group = self.alias_groups.get(group_id).ok_or_else(|| {
            AliasValidationError::UnknownAliasGroup {
                group_id: group_id.to_owned(),
            }
        })?;

        let missing = participants
            .into_iter()
            .filter(|user_id| !group.aliases.contains_key(user_id))
            .copied()
            .collect::<Vec<_>>();

        if missing.is_empty() {
            Ok(())
        } else {
            Err(AliasValidationError::MissingAliases {
                group_id: group_id.to_owned(),
                user_ids: missing,
            })
        }
    }

    pub fn guild_has_alias_group(&self, guild_id: GuildId, group_id: &str) -> bool {
        self.guilds
            .get(&guild_id)
            .is_some_and(|guild| guild.alias_groups.iter().any(|id| id == group_id))
    }
}

impl ChronicleConfig {
    fn validate(&self) -> Result<()> {
        if self.corpus_dir.trim().is_empty() {
            bail!("Chronicle corpus_dir cannot be empty");
        }
        if self.llm_repo.trim().is_empty()
            || self.llm_revision.trim().is_empty()
            || self.llm_model_file.trim().is_empty()
            || self.llm_tokenizer_repo.trim().is_empty()
            || self.llm_tokenizer_file.trim().is_empty()
        {
            bail!("Chronicle LLM repository and file settings cannot be empty");
        }
        if self.llm_system_prompt.trim().is_empty() {
            bail!("Chronicle llm_system_prompt cannot be empty");
        }
        if self.llm_max_tokens == 0 || self.llm_max_tokens > 32_768 {
            bail!("Chronicle llm_max_tokens must be between 1 and 32768");
        }
        let llm_max_tokens = usize::try_from(self.llm_max_tokens)
            .context("Chronicle llm_max_tokens does not fit in usize")?;
        if self.llm_context_limit <= llm_max_tokens || self.llm_context_limit > 32_768 {
            bail!(
                "Chronicle llm_context_limit must be greater than llm_max_tokens and no greater than 32768"
            );
        }
        if self.llm_max_reply_length == 0 || self.llm_max_reply_length > MESSAGE_MAX_CHARS {
            bail!("Chronicle llm_max_reply_length must be between 1 and {MESSAGE_MAX_CHARS}");
        }
        if !self.llm_temperature.is_finite() || !(0.0..=2.0).contains(&self.llm_temperature) {
            bail!("Chronicle llm_temperature must be finite and between 0.0 and 2.0");
        }
        if self.retrieval_limit == 0 || self.retrieval_limit > 100 {
            bail!("Chronicle retrieval_limit must be between 1 and 100");
        }
        if self.retrieval_candidate_limit < self.retrieval_limit
            || self.retrieval_candidate_limit > 1000
        {
            bail!("Chronicle retrieval_candidate_limit must be between retrieval_limit and 1000");
        }
        if !self.retrieval_distance_threshold.is_finite() || self.retrieval_distance_threshold < 0.0
        {
            bail!("Chronicle retrieval_distance_threshold must be finite and non-negative");
        }
        if !self.retrieval_near_duplicate_threshold.is_finite()
            || !(0.0..=1.0).contains(&self.retrieval_near_duplicate_threshold)
        {
            bail!(
                "Chronicle retrieval_near_duplicate_threshold must be finite and between 0.0 and 1.0"
            );
        }
        if self.retrieval_max_chunks_per_document == 0 {
            bail!("Chronicle retrieval_max_chunks_per_document must be greater than zero");
        }
        if !(3..=512).contains(&self.max_chunk_tokens) {
            bail!("Chronicle max_chunk_tokens must be between 3 and 512");
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum AliasValidationError {
    UnknownAliasGroup {
        group_id: String,
    },
    MissingAliases {
        group_id: String,
        user_ids: Vec<UserId>,
    },
}

impl std::fmt::Display for AliasValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownAliasGroup { group_id } => {
                write!(f, "unknown alias group `{group_id}`")
            }

            Self::MissingAliases { group_id, user_ids } => {
                write!(f, "alias group `{group_id}` is missing aliases for users: ")?;

                for (index, user_id) in user_ids.iter().enumerate() {
                    if index > 0 {
                        write!(f, ", ")?;
                    }

                    write!(f, "{}", user_id.get())?;
                }

                Ok(())
            }
        }
    }
}

impl std::error::Error for AliasValidationError {}

fn parse_user_id(value: &str) -> Result<UserId> {
    let id = value
        .parse::<u64>()
        .with_context(|| format!("`{value}` is not a valid Discord user ID"))?;

    if id == 0 {
        bail!("Discord user ID cannot be zero");
    }

    Ok(UserId::new(id))
}

fn parse_guild_id(value: &str) -> Result<GuildId> {
    let id = value
        .parse::<u64>()
        .with_context(|| format!("`{value}` is not a valid Discord guild ID"))?;

    if id == 0 {
        bail!("Discord guild ID cannot be zero");
    }

    Ok(GuildId::new(id))
}
