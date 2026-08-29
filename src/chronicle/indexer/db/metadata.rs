use anyhow::{Context, Result};
use sqlx::SqlitePool;

pub const SCHEMA_VERSION: &str = "1";
const SCHEMA: &str = include_str!("../../../../database/chronicle.sql");

pub async fn initialise(pool: &SqlitePool) -> Result<()> {
    sqlx::query(SCHEMA)
        .execute(pool)
        .await
        .context("Failed to initialise Chronicle index schema")?;

    set_metadata(pool, "schema_version", SCHEMA_VERSION).await?;

    Ok(())
}

pub async fn set_metadata(pool: &SqlitePool, key: &str, value: &str) -> Result<()> {
    sqlx::query(
        r"
        INSERT INTO metadata (key, value)
        VALUES (?, ?)
        ON CONFLICT(key) DO UPDATE SET value = excluded.value
        ",
    )
    .bind(key)
    .bind(value)
    .execute(pool)
    .await
    .with_context(|| format!("Failed to set index metadata: {key}"))?;

    Ok(())
}
