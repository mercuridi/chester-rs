use std::{fs, path::Path};

use anyhow::{Context, Result};

use super::{
    app::Config,
    chronicle::ChronicleConfig,
    database::DatabaseConfig,
    discord::DiscordConfig,
    paths::{AppPaths, project_root},
    raw::RawConfig,
};

pub(crate) fn load(path: &Path) -> Result<Config> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("Failed to read config file {}", path.display()))?;
    let raw: RawConfig = toml::from_str(&contents)
        .with_context(|| format!("Failed to parse config file {}", path.display()))?;
    let root = project_root(path);
    let gm_user_ids = raw.chronicle.gm_user_ids.clone();
    let discord = DiscordConfig::from_raw(raw.alias_groups, raw.guilds, gm_user_ids)?;
    Ok(Config::new(
        DatabaseConfig::from_raw(&raw.database, root)?,
        ChronicleConfig::from_raw(raw.chronicle, root)?,
        AppPaths::from_project_root(root),
        discord,
    ))
}
