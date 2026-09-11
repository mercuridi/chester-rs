use std::path::Path;

use anyhow::Result;
use serenity::all::{GuildId, UserId};

use super::{
    chronicle::ChronicleConfig,
    database::DatabaseConfig,
    discord::{AliasGroup, AliasValidationError, DiscordConfig},
    paths::AppPaths,
};

#[derive(Debug)]
pub struct Config {
    pub database: DatabaseConfig,
    pub chronicle: ChronicleConfig,
    pub paths: AppPaths,
    discord: DiscordConfig,
}

impl Config {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        super::loader::load(path.as_ref())
    }
    pub fn alias_group(&self, group_id: &str) -> Option<&AliasGroup> {
        self.discord.alias_group(group_id)
    }
    pub fn alias_groups_for_guild(&self, guild_id: GuildId) -> Option<Vec<(&str, &AliasGroup)>> {
        self.discord.alias_groups_for_guild(guild_id)
    }
    pub fn validate_participants<'a>(
        &self,
        group_id: &str,
        participants: impl IntoIterator<Item = &'a UserId>,
    ) -> Result<(), AliasValidationError> {
        self.discord.validate_participants(group_id, participants)
    }
    pub fn guild_has_alias_group(&self, guild_id: GuildId, group_id: &str) -> bool {
        self.discord.guild_has_alias_group(guild_id, group_id)
    }
    pub fn is_chronicle_gm(&self, user_id: UserId) -> bool {
        self.chronicle.access.is_gm(user_id)
    }
    pub(crate) fn new(
        database: DatabaseConfig,
        chronicle: ChronicleConfig,
        paths: AppPaths,
        discord: DiscordConfig,
    ) -> Self {
        Self {
            database,
            chronicle,
            paths,
            discord,
        }
    }
}
