use anyhow::{Context, Result};
use sqlx::{Row, SqlitePool, sqlite::SqliteConnectOptions};
use std::{path::Path, str::FromStr};

pub async fn open_sqlite_pool(database_url: &str, database_name: &str) -> Result<SqlitePool> {
    let options = SqliteConnectOptions::from_str(database_url)
        .with_context(|| format!("Failed to parse {database_name} database URL: {database_url}"))?;

    if let Some(parent) = options.get_filename().parent()
        && !parent.as_os_str().is_empty()
        && parent != Path::new(".")
    {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "Failed to create {database_name} database directory: {}",
                parent.display()
            )
        })?;
    }

    let pool = SqlitePool::connect_with(options.create_if_missing(true))
        .await
        .with_context(|| format!("Failed to open {database_name} database: {database_url}"))?;

    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&pool)
        .await
        .with_context(|| format!("Failed to enable {database_name} SQLite foreign keys"))?;

    let rows = sqlx::query("PRAGMA database_list")
        .fetch_all(&pool)
        .await
        .with_context(|| format!("Failed to inspect {database_name} database"))?;

    for row in rows {
        let name: String = row.get("name");
        let file: String = row.get("file");
        tracing::info!(database = %name, file = %file, database_name, "SQLite database opened");
    }

    Ok(pool)
}
