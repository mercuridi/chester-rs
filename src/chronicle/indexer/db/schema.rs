use anyhow::{Context, Result};
use sqlx::{Row, SqlitePool};

const SCHEMA: &str = include_str!("../../../../database/chronicle.sql");

pub async fn initialise(pool: &SqlitePool) -> Result<()> {
    sqlx::query(SCHEMA)
        .execute(pool)
        .await
        .context("Failed to initialise Chronicle index schema")?;

    let columns = sqlx::query("PRAGMA table_info(chunks)")
        .fetch_all(pool)
        .await
        .context("Failed to inspect Chronicle chunk schema")?;
    let has_overlap_column = columns.iter().any(|column| {
        column
            .try_get::<String, _>("name")
            .is_ok_and(|name| name == "overlaps_previous")
    });
    if !has_overlap_column {
        sqlx::query("ALTER TABLE chunks ADD COLUMN overlaps_previous INTEGER NOT NULL DEFAULT 0")
            .execute(pool)
            .await
            .context("Failed to migrate Chronicle chunk overlap metadata")?;
    }

    Ok(())
}
