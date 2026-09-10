use anyhow::{Result, bail};
use std::path::Path;

use super::{paths::resolve_sqlite_url, raw::RawDatabaseConfig};

#[derive(Debug)]
pub struct DatabaseConfig {
    pub jester: String,
    pub chronicle: String,
}

impl DatabaseConfig {
    pub(crate) fn from_raw(raw: &RawDatabaseConfig, project_root: &Path) -> Result<Self> {
        if raw.jester.trim().is_empty() || raw.chronicle.trim().is_empty() {
            bail!("Database URLs cannot be empty");
        }
        Ok(Self {
            jester: resolve_sqlite_url(project_root, &raw.jester),
            chronicle: resolve_sqlite_url(project_root, &raw.chronicle),
        })
    }
}
