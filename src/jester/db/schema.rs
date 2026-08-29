use anyhow::{Context, Result};
use sqlx::SqlitePool;

const SCHEMA: &str = include_str!("../../../database/jester.sql");

pub async fn initialise(pool: &SqlitePool) -> Result<()> {
    sqlx::query(SCHEMA)
        .execute(pool)
        .await
        .context("Failed to initialise Jester database schema")?;

    Ok(())
}
