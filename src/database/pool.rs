use anyhow::{Context, Result};
use sqlx::{Row, SqlitePool};

pub async fn open_sqlite_pool(database_url: &str, database_name: &str) -> Result<SqlitePool> {
    let pool = SqlitePool::connect(database_url)
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
