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

    // External-content FTS is maintained inside the same transactions as chunks.
    sqlx::raw_sql("CREATE VIRTUAL TABLE IF NOT EXISTS chunk_fts USING fts5(heading, text, content='chunks', content_rowid='id');
        CREATE TRIGGER IF NOT EXISTS chunks_fts_insert AFTER INSERT ON chunks BEGIN
            INSERT INTO chunk_fts(rowid, heading, text) VALUES (new.id, new.heading, new.text);
        END;
        CREATE TRIGGER IF NOT EXISTS chunks_fts_delete AFTER DELETE ON chunks BEGIN
            INSERT INTO chunk_fts(chunk_fts, rowid, heading, text) VALUES ('delete', old.id, old.heading, old.text);
        END;
        CREATE TRIGGER IF NOT EXISTS chunks_fts_update AFTER UPDATE ON chunks BEGIN
            INSERT INTO chunk_fts(chunk_fts, rowid, heading, text) VALUES ('delete', old.id, old.heading, old.text);
            INSERT INTO chunk_fts(rowid, heading, text) VALUES (new.id, new.heading, new.text);
        END;")
        .execute(pool).await.context("Failed to initialise FTS5")?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS note_metadata (
        document_id INTEGER PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE,
        note_id TEXT NOT NULL, note_type TEXT NOT NULL, status TEXT NOT NULL,
        visibility TEXT NOT NULL, aliases TEXT NOT NULL, summary TEXT NOT NULL,
        role TEXT, character_status TEXT)",
    )
    .execute(pool)
    .await?;
    let columns = sqlx::query("PRAGMA table_info(note_metadata)")
        .fetch_all(pool)
        .await?;
    for field in ["role", "character_status"] {
        if !columns
            .iter()
            .any(|column| column.get::<String, _>("name") == field)
        {
            // Identifiers are the two fixed column names above, never model output.
            sqlx::query(&format!(
                "ALTER TABLE note_metadata ADD COLUMN {field} TEXT"
            ))
            .execute(pool)
            .await?;
        }
    }
    sqlx::query("CREATE INDEX IF NOT EXISTS note_metadata_selection ON note_metadata(status, note_type, role, character_status)")
        .execute(pool).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::initialise;
    use sqlx::{Row, SqlitePool};
    use tempfile::tempdir;

    #[tokio::test]
    async fn adds_character_columns_to_existing_metadata_without_losing_values()
    -> anyhow::Result<()> {
        let temp = tempdir()?;
        super::super::repository::register_sqlite_vec();
        let pool = crate::database::pool::open_sqlite_pool(
            &format!("sqlite://{}", temp.path().join("legacy.sqlite3").display()),
            "test",
        )
        .await?;
        sqlx::query(super::SCHEMA).execute(&pool).await?;
        sqlx::raw_sql("CREATE TABLE note_metadata (document_id INTEGER PRIMARY KEY, note_id TEXT NOT NULL, note_type TEXT NOT NULL, status TEXT NOT NULL, visibility TEXT NOT NULL, aliases TEXT NOT NULL, summary TEXT NOT NULL);
            INSERT INTO note_metadata VALUES (1, 'ada', 'character', 'canon', 'player', '[]', 'A gardener');")
            .execute(&pool).await?;
        initialise(&pool).await?;
        let row = sqlx::query("SELECT note_id, summary, role, character_status FROM note_metadata")
            .fetch_one(&pool)
            .await?;
        assert_eq!(row.get::<String, _>("summary"), "A gardener");
        assert_eq!(row.get::<String, _>("note_id"), "ada");
        assert!(row.get::<Option<String>, _>("role").is_none());
        assert!(row.get::<Option<String>, _>("character_status").is_none());
        Ok(())
    }

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
