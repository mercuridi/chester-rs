use anyhow::{Context, Result};
use sqlx::SqlitePool;

const SCHEMA: &str = include_str!("../../../database/jester.sql");

pub async fn initialise(pool: &SqlitePool) -> Result<()> {
    sqlx::raw_sql(SCHEMA)
        .execute(pool)
        .await
        .context("Failed to initialize the Jester schema")?;
    Ok(())
}
