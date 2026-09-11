use std::{fs, path::Path};

use anyhow::{Context, Result};
use serde::Deserialize;

use super::{
    app::Config,
    chronicle::ChronicleConfig,
    database::{DatabaseConfig, FileDatabaseConfig},
    discord::DiscordConfig,
    paths::{AppPaths, project_root},
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    database: FileDatabaseConfig,
    chronicle: super::chronicle::FileChronicleConfig,
    discord: super::discord::FileDiscordConfig,
}

pub(crate) fn load(path: &Path) -> Result<Config> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("Failed to read config file {}", path.display()))?;
    let file: FileConfig = toml::from_str(&contents)
        .with_context(|| format!("Failed to parse config file {}", path.display()))?;
    let root = project_root(path);
    let discord = DiscordConfig::from_file(file.discord)?;
    Ok(Config::new(
        DatabaseConfig::from_file(&file.database, root)?,
        ChronicleConfig::from_file(file.chronicle, root)?,
        AppPaths::from_project_root(root),
        discord,
    ))
}
