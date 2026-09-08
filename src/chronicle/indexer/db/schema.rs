use anyhow::{Context, Result};
use sqlx::SqlitePool;

/// Bump whenever any stored Chronicle index output changes, including this
/// schema, embedding dimensions, chunking, or retrieval-index semantics.
pub const INDEX_FORMAT_VERSION: u32 = 4;

const SCHEMA: &str = include_str!("../../../../database/chronicle.sql");

/// Creates a fresh Chronicle index. Existing databases are intentionally not
/// migrated; their versioned filename ensures they are not opened.
pub async fn initialise(pool: &SqlitePool) -> Result<()> {
    sqlx::raw_sql(SCHEMA)
        .execute(pool)
        .await
        .context("Failed to create Chronicle index schema")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use sqlx::Row;

    #[tokio::test]
    async fn schema_file_creates_the_complete_index() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        super::super::repository::register_sqlite_vec();
        let pool = crate::database::pool::open_sqlite_pool(
            &format!(
                "sqlite://{}",
                directory.path().join("index.sqlite3").display()
            ),
            "test",
        )
        .await?;

        super::initialise(&pool).await?;
        super::initialise(&pool).await?;

        for name in [
            "documents",
            "chunks",
            "chunk_embeddings_player",
            "chunk_embeddings_secret",
            "chunk_fts",
            "note_metadata",
            "adventure_metadata",
            "aspect_metadata",
            "character_metadata",
            "deity_metadata",
            "event_metadata",
            "language_metadata",
            "location_metadata",
            "lore_metadata",
            "metagame_metadata",
            "monster_metadata",
            "object_metadata",
            "organisation_metadata",
            "race_metadata",
            "note_wikilinks",
            "note_string_lists",
            "note_scalar_fields",
        ] {
            let count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM sqlite_master WHERE (type = 'table' OR type = 'index') AND name = ?",
            )
            .bind(name)
            .fetch_one(&pool)
            .await?;
            assert_eq!(count, 1, "schema did not create {name}");
        }

        let trigger_names = sqlx::query("SELECT name FROM sqlite_master WHERE type = 'trigger'")
            .fetch_all(&pool)
            .await?
            .into_iter()
            .map(|row| row.get::<String, _>("name"))
            .collect::<Vec<_>>();
        assert!(trigger_names.contains(&"chunks_fts_insert".to_owned()));
        assert!(trigger_names.contains(&"chunks_fts_delete".to_owned()));
        assert!(trigger_names.contains(&"chunks_fts_update".to_owned()));
        Ok(())
    }
}
