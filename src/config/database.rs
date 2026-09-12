use anyhow::{Result, bail};
use serde::Deserialize;
use std::path::Path;

use super::paths::resolve_sqlite_url;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileDatabaseConfig {
    jester: String,
    chronicle: String,
}

#[derive(Debug)]
pub struct DatabaseConfig {
    pub jester: String,
    pub chronicle: String,
}

impl DatabaseConfig {
    pub(crate) fn from_file(file: &FileDatabaseConfig, project_root: &Path) -> Result<Self> {
        if file.jester.trim().is_empty() || file.chronicle.trim().is_empty() {
            bail!("Database URLs cannot be empty");
        }
        Ok(Self {
            jester: resolve_sqlite_url(project_root, &file.jester),
            chronicle: resolve_sqlite_url(project_root, &file.chronicle),
        })
    }
}
