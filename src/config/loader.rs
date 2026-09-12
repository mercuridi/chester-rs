use std::fs;

use anyhow::{Context, Result};
use serde::Deserialize;

use super::{
    app::{Config, LoggingConfig},
    chronicle::ChronicleConfig,
    database::{DatabaseConfig, FileDatabaseConfig},
    discord::DiscordConfig,
    paths::AppPaths,
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    database: FileDatabaseConfig,
    chronicle: super::chronicle::FileChronicleConfig,
    discord: super::discord::FileDiscordConfig,
    logging: FileLoggingConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileLoggingConfig {
    #[serde(default)]
    content: bool,
}

pub(crate) fn load(paths: AppPaths) -> Result<Config> {
    let contents = fs::read_to_string(&paths.config_path)
        .with_context(|| format!("Failed to read config file {}", paths.config_path.display()))?;
    let file: FileConfig = toml::from_str(&contents).with_context(|| {
        format!(
            "Failed to parse config file {}",
            paths.config_path.display()
        )
    })?;
    let discord = DiscordConfig::from_file(file.discord)?;
    Ok(Config::new(
        DatabaseConfig::from_file(&file.database, &paths.runtime_root)?,
        ChronicleConfig::from_file(file.chronicle, &paths.runtime_root)?,
        LoggingConfig {
            content: file.logging.content,
        },
        paths,
        discord,
    ))
}
