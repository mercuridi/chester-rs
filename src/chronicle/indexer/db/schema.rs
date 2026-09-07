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
        visibility TEXT NOT NULL, aliases TEXT NOT NULL, tags TEXT NOT NULL,
        summary TEXT NOT NULL, created TEXT NOT NULL, updated TEXT NOT NULL,
        role TEXT, character_status TEXT)",
    )
    .execute(pool)
    .await?;
    let columns = sqlx::query("PRAGMA table_info(note_metadata)")
        .fetch_all(pool)
        .await?;
    for field in ["tags", "created", "updated", "role", "character_status"] {
        if !columns
            .iter()
            .any(|column| column.get::<String, _>("name") == field)
        {
            let definition = match field {
                "tags" => "TEXT NOT NULL DEFAULT '[]'",
                "created" | "updated" => "TEXT NOT NULL DEFAULT ''",
                "role" | "character_status" => "TEXT",
                _ => unreachable!("field is listed above"),
            };
            sqlx::query(&format!(
                "ALTER TABLE note_metadata ADD COLUMN {field} {definition}"
            ))
            .execute(pool)
            .await?;
        }
    }
    sqlx::raw_sql(
        "CREATE TABLE IF NOT EXISTS adventure_metadata (
            document_id INTEGER PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE,
            adventure_status TEXT, start_date TEXT, end_date TEXT,
            system TEXT, part_of_adventure TEXT, level_range TEXT
        );
        CREATE TABLE IF NOT EXISTS aspect_metadata (
            document_id INTEGER PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS character_metadata (
            document_id INTEGER PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE,
            race TEXT, role TEXT, character_status TEXT, location TEXT,
            birthplace TEXT, nationality TEXT, played_by TEXT, pronouns TEXT
        );
        CREATE TABLE IF NOT EXISTS deity_metadata (
            document_id INTEGER PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE,
            pantheon TEXT, domain TEXT, antidomain TEXT, alignment TEXT,
            form TEXT, crystal TEXT
        );
        CREATE TABLE IF NOT EXISTS event_metadata (
            document_id INTEGER PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE,
            event_type TEXT, occurred TEXT, occurred_start TEXT,
            occurred_end TEXT, historicity TEXT, result TEXT
        );
        CREATE TABLE IF NOT EXISTS language_metadata (
            document_id INTEGER PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS location_metadata (
            document_id INTEGER PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE,
            location_type TEXT, contained_in TEXT, population TEXT, demonym TEXT
        );
        CREATE TABLE IF NOT EXISTS lore_metadata (
            document_id INTEGER PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE,
            lore_type TEXT, common_knowledge INTEGER
        );
        CREATE TABLE IF NOT EXISTS metagame_metadata (
            document_id INTEGER PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE,
            category TEXT, system TEXT, session_date TEXT
        );
        CREATE TABLE IF NOT EXISTS monster_metadata (
            document_id INTEGER PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE,
            creature_type TEXT, threat_level TEXT, alignment TEXT, size TEXT,
            source_inspiration TEXT
        );
        CREATE TABLE IF NOT EXISTS object_metadata (
            document_id INTEGER PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE,
            object_type TEXT, rarity TEXT, owner TEXT, location TEXT,
            creator TEXT, attunement TEXT
        );
        CREATE TABLE IF NOT EXISTS organisation_metadata (
            document_id INTEGER PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE,
            organisation_type TEXT, leader TEXT, founder TEXT, headquarters TEXT,
            founded TEXT, dissolved TEXT, motto TEXT
        );
        CREATE TABLE IF NOT EXISTS race_metadata (
            document_id INTEGER PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE,
            lifespan TEXT, playable INTEGER
        );
        CREATE TABLE IF NOT EXISTS note_wikilinks (
            document_id INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
            field_name TEXT NOT NULL,
            position INTEGER NOT NULL,
            value TEXT NOT NULL,
            PRIMARY KEY (document_id, field_name, position)
        );
        CREATE TABLE IF NOT EXISTS note_string_lists (
            document_id INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
            field_name TEXT NOT NULL,
            position INTEGER NOT NULL,
            value TEXT NOT NULL,
            PRIMARY KEY (document_id, field_name, position)
        );
        CREATE INDEX IF NOT EXISTS note_wikilinks_lookup
            ON note_wikilinks(field_name, value);
        CREATE INDEX IF NOT EXISTS note_string_lists_lookup
            ON note_string_lists(field_name, value);",
    )
    .execute(pool)
    .await?;
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
        let row = sqlx::query("SELECT note_id, summary, tags, created, updated, role, character_status FROM note_metadata")
            .fetch_one(&pool)
            .await?;
        assert_eq!(row.get::<String, _>("summary"), "A gardener");
        assert_eq!(row.get::<String, _>("note_id"), "ada");
        assert_eq!(row.get::<String, _>("tags"), "[]");
        assert_eq!(row.get::<String, _>("created"), "");
        assert_eq!(row.get::<String, _>("updated"), "");
        assert!(row.get::<Option<String>, _>("role").is_none());
        assert!(row.get::<Option<String>, _>("character_status").is_none());
        for table in [
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
        ] {
            assert_eq!(
                sqlx::query_scalar::<_, i64>(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?"
                )
                .bind(table)
                .fetch_one(&pool)
                .await?,
                1,
                "missing table {table}"
            );
        }
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
