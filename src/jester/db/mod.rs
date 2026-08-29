use anyhow::{Context, Result};
use sqlx::SqlitePool;

pub mod metadata;
pub mod repository;

const SCHEMA: &str = include_str!("../../../database/jester.sql");
pub const SCHEMA_VERSION: &str = "1";

pub async fn initialise(pool: &SqlitePool) -> Result<()> {
    sqlx::query(SCHEMA)
        .execute(pool)
        .await
        .context("Failed to initialise Jester database schema")?;

    sqlx::query(
        r"
        INSERT INTO metadata (key, value)
        VALUES ('schema_version', ?)
        ON CONFLICT(key) DO UPDATE SET value = excluded.value
        ",
    )
    .bind(SCHEMA_VERSION)
    .execute(pool)
    .await
    .context("Failed to set Jester database schema version")?;

    Ok(())
}
