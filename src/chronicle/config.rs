use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serenity::all::{GuildId, UserId};

pub type AliasGroupId = String;

const DEFAULT_INDEX_DB: &str = "sqlite://database/chronicle/chronicle.sqlite3";
const DEFAULT_CORPUS_DIR: &str = "corpus";
const DEFAULT_LLM_URL: &str = "http://127.0.0.1:8080";
const DEFAULT_LLM_MODEL: &str = "Qwen2.5-7B-Instruct";
const DEFAULT_LLM_MAX_TOKENS: u32 = 512;
const DEFAULT_LLM_TEMPERATURE: f32 = 0.2;
const DEFAULT_RETRIEVAL_LIMIT: usize = 5;
const DEFAULT_MAX_CHUNK_LENGTH: usize = 2_000;

fn default_index_db() -> String {
    DEFAULT_INDEX_DB.to_owned()
}

fn default_llm_url() -> String {
    DEFAULT_LLM_URL.to_owned()
}

fn default_llm_model() -> String {
    DEFAULT_LLM_MODEL.to_owned()
}

#[derive(Debug, Deserialize)]
struct RawConfig {
    #[serde(default)]
    alias_groups: HashMap<String, RawAliasGroup>,

    #[serde(default)]
    guilds: HashMap<String, RawGuildConfig>,

    #[serde(default)]
    chronicle: RawChronicleConfig,
}

#[derive(Debug, Deserialize, Default)]
pub struct RawChronicleConfig {
    #[serde(default = "default_index_db")]
    index_db: String,

    #[serde(default = "default_llm_url")]
    llm_url: String,

    #[serde(default = "default_llm_model")]
    llm_model: String,

    #[serde(default = "default_corpus_dir")]
    corpus_dir: String,

    #[serde(default = "default_llm_max_tokens")]
    llm_max_tokens: u32,

    #[serde(default = "default_llm_temperature")]
    llm_temperature: f32,

    #[serde(default = "default_retrieval_limit")]
    retrieval_limit: usize,

    #[serde(default = "default_max_chunk_length")]
    max_chunk_length: usize,
}

fn default_corpus_dir() -> String { DEFAULT_CORPUS_DIR.to_owned() }
fn default_llm_max_tokens() -> u32 { DEFAULT_LLM_MAX_TOKENS }
fn default_llm_temperature() -> f32 { DEFAULT_LLM_TEMPERATURE }
fn default_retrieval_limit() -> usize { DEFAULT_RETRIEVAL_LIMIT }
fn default_max_chunk_length() -> usize { DEFAULT_MAX_CHUNK_LENGTH }

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
    pub chronicle: ChronicleConfig,
}

#[derive(Debug, Clone)]
pub struct ChronicleConfig {
    pub index_db: String,
    pub llm_url: String,
    pub llm_model: String,
    pub corpus_dir: String,
    pub llm_max_tokens: u32,
    pub llm_temperature: f32,
    pub retrieval_limit: usize,
    pub max_chunk_length: usize,
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

impl Config {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();

        let contents = fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file {}", path.display()))?;

        let raw: RawConfig = toml::from_str(&contents)
            .with_context(|| format!("Failed to parse config file {}", path.display()))?;

        Self::from_raw(raw)
    }

    fn from_raw(raw: RawConfig) -> Result<Self> {
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
                let user_id = parse_user_id(&raw_user_id)
                    .with_context(|| {
                        format!(
                            "Invalid user ID `{raw_user_id}` in alias group `{group_id}`"
                        )
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
                    bail!(
                        "Guild `{raw_guild_id}` references unknown alias group `{group_id}`"
                    );
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
            index_db: raw.chronicle.index_db,
            llm_url: raw.chronicle.llm_url,
            llm_model: raw.chronicle.llm_model,
            corpus_dir: raw.chronicle.corpus_dir,
            llm_max_tokens: raw.chronicle.llm_max_tokens,
            llm_temperature: raw.chronicle.llm_temperature,
            retrieval_limit: raw.chronicle.retrieval_limit,
            max_chunk_length: raw.chronicle.max_chunk_length,
        };

        chronicle.validate()?;

        Ok(Self {
            alias_groups,
            guilds,
            chronicle,
        })
    }

    pub fn alias_group(&self, group_id: &str) -> Option<&AliasGroup> {
        self.alias_groups.get(group_id)
    }

    pub fn alias_groups_for_guild(
        &self,
        guild_id: GuildId,
    ) -> Option<Vec<(&str, &AliasGroup)>> {
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
        let group = self
            .alias_groups
            .get(group_id)
            .ok_or_else(|| AliasValidationError::UnknownAliasGroup {
                group_id: group_id.to_owned(),
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

    pub fn guild_has_alias_group(
        &self,
        guild_id: GuildId,
        group_id: &str,
    ) -> bool {
        self.guilds
            .get(&guild_id)
            .is_some_and(|guild| guild.alias_groups.iter().any(|id| id == group_id))
    }

}

impl ChronicleConfig {
    fn validate(&self) -> Result<()> {
        if self.index_db.trim().is_empty() || self.corpus_dir.trim().is_empty() {
            bail!("Chronicle index_db and corpus_dir cannot be empty");
        }
        if self.llm_model.trim().is_empty() {
            bail!("Chronicle llm_model cannot be empty");
        }
        let url = url::Url::parse(&self.llm_url)
            .with_context(|| format!("Invalid Chronicle llm_url `{}`", self.llm_url))?;
        if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
            bail!("Chronicle llm_url must be an HTTP(S) URL with a host");
        }
        if self.llm_max_tokens == 0 || self.llm_max_tokens > 32_768 {
            bail!("Chronicle llm_max_tokens must be between 1 and 32768");
        }
        if !self.llm_temperature.is_finite() || !(0.0..=2.0).contains(&self.llm_temperature) {
            bail!("Chronicle llm_temperature must be finite and between 0.0 and 2.0");
        }
        if self.retrieval_limit == 0 || self.retrieval_limit > 100 {
            bail!("Chronicle retrieval_limit must be between 1 and 100");
        }
        if self.max_chunk_length == 0 {
            bail!("Chronicle max_chunk_length must be greater than zero");
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

            Self::MissingAliases {
                group_id,
                user_ids,
            } => {
                write!(
                    f,
                    "alias group `{group_id}` is missing aliases for users: "
                )?;

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
