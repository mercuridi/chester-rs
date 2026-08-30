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

#[cfg(test)]
mod tests {
    use super::initialise;
    use sqlx::{Row, SqlitePool};
    use tempfile::tempdir;

    #[tokio::test]
    async fn migrates_legacy_chunks_table_and_remains_idempotent() -> anyhow::Result<()> {
        let directory = tempdir()?;
        let url = format!(
            "sqlite://{}",
            directory.path().join("chronicle.db").display()
        );
        super::super::repository::register_sqlite_vec();
        let pool: SqlitePool = crate::database::pool::open_sqlite_pool(&url, "test").await?;
        sqlx::query(
            "CREATE TABLE chunks (
                id INTEGER PRIMARY KEY,
                document_id INTEGER NOT NULL,
                chunk_index INTEGER NOT NULL,
                heading TEXT,
                text TEXT NOT NULL,
                UNIQUE (document_id, chunk_index)
            )",
        )
        .execute(&pool)
        .await?;

        initialise(&pool).await?;
        initialise(&pool).await?;

        let columns = sqlx::query("PRAGMA table_info(chunks)")
            .fetch_all(&pool)
            .await?;
        assert!(columns.iter().any(|column| {
            column
                .try_get::<String, _>("name")
                .is_ok_and(|name| name == "overlaps_previous")
        }));
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM documents")
                .fetch_one(&pool)
                .await?,
            0
        );
        Ok(())
    }
}
