use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result, bail};
use serenity::all::{GuildId, UserId};

use super::raw::{RawAliasGroup, RawGuildConfig};

pub type AliasGroupId = String;

#[derive(Debug)]
pub struct AliasGroup {
    pub name: String,
    pub aliases: HashMap<UserId, String>,
}

#[derive(Debug)]
pub struct GuildConfig {
    pub alias_groups: Vec<AliasGroupId>,
}

#[derive(Debug)]
pub struct DiscordConfig {
    alias_groups: HashMap<AliasGroupId, AliasGroup>,
    guilds: HashMap<GuildId, GuildConfig>,
    chronicle_gm_user_ids: HashSet<UserId>,
}

impl DiscordConfig {
    pub(crate) fn from_raw(
        raw_groups: HashMap<String, RawAliasGroup>,
        raw_guilds: HashMap<String, RawGuildConfig>,
        raw_gm_user_ids: Vec<String>,
    ) -> Result<Self> {
        let alias_groups = build_alias_groups(raw_groups)?;
        let guilds = build_guilds(raw_guilds, &alias_groups)?;
        Ok(Self {
            alias_groups,
            guilds,
            chronicle_gm_user_ids: parse_gm_user_ids(raw_gm_user_ids)?,
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
    pub fn is_chronicle_gm(&self, user_id: UserId) -> bool {
        self.chronicle_gm_user_ids.contains(&user_id)
    }
}

fn build_alias_groups(
    raw_groups: HashMap<String, RawAliasGroup>,
) -> Result<HashMap<String, AliasGroup>> {
    raw_groups.into_iter().map(|(group_id, raw_group)| {
        if group_id.trim().is_empty() { bail!("Alias group ID cannot be empty"); }
        if raw_group.name.trim().is_empty() { bail!("Alias group `{group_id}` has an empty name"); }
        let aliases = raw_group.aliases.into_iter().map(|(raw_user_id, alias)| {
            let user_id = parse_user_id(&raw_user_id).with_context(|| format!("Invalid user ID `{raw_user_id}` in alias group `{group_id}`"))?;
            if alias.trim().is_empty() { bail!("Alias for user `{raw_user_id}` in alias group `{group_id}` cannot be empty"); }
            Ok((user_id, alias))
        }).collect::<Result<HashMap<_, _>>>()?;
        Ok((group_id, AliasGroup { name: raw_group.name, aliases }))
    }).collect()
}

fn build_guilds(
    raw_guilds: HashMap<String, RawGuildConfig>,
    alias_groups: &HashMap<String, AliasGroup>,
) -> Result<HashMap<GuildId, GuildConfig>> {
    raw_guilds
        .into_iter()
        .map(|(raw_guild_id, raw_guild)| {
            let guild_id = parse_guild_id(&raw_guild_id)
                .with_context(|| format!("Invalid guild ID `{raw_guild_id}`"))?;
            for group_id in &raw_guild.alias_groups {
                if !alias_groups.contains_key(group_id) {
                    bail!("Guild `{raw_guild_id}` references unknown alias group `{group_id}`");
                }
            }
            Ok((
                guild_id,
                GuildConfig {
                    alias_groups: raw_guild.alias_groups,
                },
            ))
        })
        .collect()
}

fn parse_gm_user_ids(raw_user_ids: Vec<String>) -> Result<HashSet<UserId>> {
    let mut user_ids = HashSet::new();
    for raw_user_id in raw_user_ids {
        let user_id = parse_user_id(&raw_user_id)
            .with_context(|| format!("Invalid Chronicle GM user ID `{raw_user_id}`"))?;
        if !user_ids.insert(user_id) {
            bail!("Duplicate Chronicle GM user ID `{raw_user_id}`");
        }
    }
    Ok(user_ids)
}

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
            Self::UnknownAliasGroup { group_id } => write!(f, "unknown alias group `{group_id}`"),
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
