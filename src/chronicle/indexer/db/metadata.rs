use anyhow::{Context, Result};
use sqlx::SqlitePool;

pub const SCHEMA_VERSION: &str = "1";

pub async fn initialise(pool: &SqlitePool) -> Result<()> {
    sqlx::query(
        r"
        CREATE TABLE IF NOT EXISTS metadata (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS documents (
            id           INTEGER PRIMARY KEY,
            path         TEXT NOT NULL UNIQUE,
            content_hash TEXT NOT NULL,
            indexed_at   TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS chunks (
            id            INTEGER PRIMARY KEY,
            document_id   INTEGER NOT NULL,
            chunk_index   INTEGER NOT NULL,
            heading       TEXT,
            text          TEXT NOT NULL,

            FOREIGN KEY (document_id)
                REFERENCES documents(id)
                ON DELETE CASCADE,

            UNIQUE (document_id, chunk_index)
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS chunk_embeddings USING vec0(
            embedding float[384]
        );
        ",
    )
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
