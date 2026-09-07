// src/chronicle/indexer/db/repository.rs

use anyhow::{Context, Result};
use chrono::Utc;
use sqlx::{Row, sqlite::SqlitePool};

#[derive(Debug, Clone)]
pub struct IndexedDocument {
    pub id: i64,
    pub path: String,
    pub content_hash: String,
}

#[derive(Debug, Clone)]
pub struct IndexedChunk {
    pub chunk_index: i64,
    pub heading: Option<String>,
    pub text: String,
    /// Whether this chunk contains content repeated from its immediate predecessor.
    pub overlaps_previous: bool,
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub document_path: String,
    pub chunk_index: i64,
    pub heading: Option<String>,
    pub text: String,
    pub overlaps_previous: bool,
    pub distance: f32,
}

#[derive(Debug, serde::Serialize)]
pub struct StructuredNote {
    pub id: String,
    pub title: String,
}
#[derive(Debug, serde::Serialize)]
pub struct StructuredResult {
    pub total: i64,
    pub notes: Vec<StructuredNote>,
}

#[derive(Clone)]
pub struct IndexerDb {
    pool: SqlitePool,
}

impl IndexerDb {
    pub async fn open(path: &str) -> Result<Self> {
        register_sqlite_vec();

        let pool = crate::database::pool::open_sqlite_pool(path, "Chronicle").await?;

        super::schema::initialise(&pool).await?;

        Ok(Self { pool })
    }

    pub async fn all_documents(&self) -> Result<Vec<IndexedDocument>> {
        let rows = sqlx::query(
            r"
            SELECT id, path, content_hash
            FROM documents
            ORDER BY path
            ",
        )
        .fetch_all(&self.pool)
        .await
        .context("Failed to query indexed documents")?;

        Ok(rows
            .into_iter()
            .map(|row| IndexedDocument {
                id: row.get("id"),
                path: row.get("path"),
                content_hash: row.get("content_hash"),
            })
            .collect())
    }

    pub async fn has_chunks(&self) -> Result<bool> {
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM chunks)")
            .fetch_one(&self.pool)
            .await
            .context("Failed to check whether the Chronicle corpus is empty")
    }

    pub async fn delete_document(&self, document_id: i64) -> Result<()> {
        let mut tx = self.pool.begin().await?;

        sqlx::query(
            r"
            DELETE FROM chunk_embeddings
            WHERE rowid IN (
                SELECT id
                FROM chunks
                WHERE document_id = ?
            )
            ",
        )
        .bind(document_id)
        .execute(&mut *tx)
        .await
        .context("Failed to delete document embeddings")?;

        sqlx::query("DELETE FROM documents WHERE id = ?")
            .bind(document_id)
            .execute(&mut *tx)
            .await
            .context("Failed to delete document")?;

        tx.commit()
            .await
            .context("Failed to commit document deletion")?;

        Ok(())
    }

    #[cfg(test)]
    pub async fn replace_document(
        &self,
        path: &str,
        content_hash: &str,
        chunks: &[IndexedChunk],
        embeddings: &[Vec<f32>],
    ) -> Result<i64> {
        self.replace_note(
            path,
            content_hash,
            chunks,
            embeddings,
            &crate::chronicle::indexer::frontmatter::Metadata::default(),
        )
        .await
    }

    pub async fn replace_note(
        &self,
        path: &str,
        content_hash: &str,
        chunks: &[IndexedChunk],
        embeddings: &[Vec<f32>],
        metadata: &crate::chronicle::indexer::frontmatter::Metadata,
    ) -> Result<i64> {
        if chunks.len() != embeddings.len() {
            anyhow::bail!(
                "Chunk/embedding count mismatch: {} chunks, {} embeddings",
                chunks.len(),
                embeddings.len()
            );
        }

        let mut tx = self.pool.begin().await?;

        let indexed_at = Utc::now().to_rfc3339();

        let document_id: i64 = sqlx::query_scalar(
            r"
            INSERT INTO documents (
                path,
                content_hash,
                indexed_at
            )
            VALUES (?, ?, ?)
            ON CONFLICT(path) DO UPDATE SET
                content_hash = excluded.content_hash,
                indexed_at = excluded.indexed_at
            RETURNING id
            ",
        )
        .bind(path)
        .bind(content_hash)
        .bind(indexed_at)
        .fetch_one(&mut *tx)
        .await
        .context("Failed to upsert indexed document")?;

        sqlx::query(
            r"
            DELETE FROM chunk_embeddings
            WHERE rowid IN (
                SELECT id
                FROM chunks
                WHERE document_id = ?
            )
            ",
        )
        .bind(document_id)
        .execute(&mut *tx)
        .await
        .context("Failed to delete existing chunk embeddings")?;

        sqlx::query("DELETE FROM chunks WHERE document_id = ?")
            .bind(document_id)
            .execute(&mut *tx)
            .await
            .context("Failed to delete existing chunks")?;

        for (chunk, embedding) in chunks.iter().zip(embeddings) {
            let chunk_id: i64 = sqlx::query_scalar(
                r"
                INSERT INTO chunks (
                    document_id,
                    chunk_index,
                    heading,
                    text,
                    overlaps_previous
                )
                VALUES (?, ?, ?, ?, ?)
                RETURNING id
                ",
            )
            .bind(document_id)
            .bind(chunk.chunk_index)
            .bind(&chunk.heading)
            .bind(&chunk.text)
            .bind(chunk.overlaps_previous)
            .fetch_one(&mut *tx)
            .await
            .context("Failed to insert chunk")?;

            let embedding_json =
                serde_json::to_string(embedding).context("Failed to serialise embedding")?;

            sqlx::query(
                r"
                INSERT INTO chunk_embeddings (
                    rowid,
                    embedding
                )
                VALUES (?, ?)
                ",
            )
            .bind(chunk_id)
            .bind(embedding_json)
            .execute(&mut *tx)
            .await
            .context("Failed to insert chunk embedding")?;
        }

        write_metadata(&mut tx, document_id, metadata).await?;

        tx.commit()
            .await
            .context("Failed to commit document replacement")?;

        Ok(document_id)
    }

    /// Refresh frontmatter independently of chunk/vector storage.
    pub async fn refresh_metadata(
        &self,
        document_id: i64,
        fingerprint: &str,
        metadata: &crate::chronicle::indexer::frontmatter::Metadata,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        write_metadata(&mut tx, document_id, metadata).await?;
        sqlx::query("UPDATE documents SET content_hash = ? WHERE id = ?")
            .bind(fingerprint)
            .bind(document_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn chunks_match(
        &self,
        document_id: i64,
        chunks: &[crate::chronicle::indexer::document::Chunk],
    ) -> Result<bool> {
        let rows = sqlx::query("SELECT chunk_index, heading, text, overlaps_previous FROM chunks WHERE document_id = ? ORDER BY chunk_index")
            .bind(document_id).fetch_all(&self.pool).await?;
        Ok(rows.len() == chunks.len()
            && rows.iter().zip(chunks).all(|(row, chunk)| {
                usize::try_from(row.get::<i64, _>("chunk_index")).ok() == Some(chunk.index)
                    && row.get::<Option<String>, _>("heading") == chunk.heading
                    && row.get::<String, _>("text") == chunk.content
                    && row.get::<bool, _>("overlaps_previous") == (chunk.overlap_tokens > 0)
            }))
    }

    pub async fn execute_plan(
        &self,
        plan: &crate::chronicle::query::plan::Plan,
    ) -> Result<StructuredResult> {
        use crate::chronicle::query::{plan::Plan, render::LIST_LIMIT};
        plan.validate()?;
        let (note_type, filters) = plan.selection().context("Plan is not a structured query")?;
        let role = filters
            .role
            .map(crate::chronicle::query::plan::CharacterRole::as_str);
        let status = filters
            .character_status
            .map(crate::chronicle::query::plan::CharacterStatus::as_str);
        let mut tx = self.pool.begin().await?;
        let total: i64 = sqlx::query_scalar("SELECT COUNT(DISTINCT note_id) FROM note_metadata
            WHERE status = 'canon' AND note_type = ? AND (? IS NULL OR role = ?) AND (? IS NULL OR character_status = ?)")
            .bind(note_type).bind(role).bind(role).bind(status).bind(status).fetch_one(&mut *tx).await?;
        let mut notes = Vec::new();
        if matches!(plan, Plan::List { .. }) {
            let rows = sqlx::query("SELECT m.note_id, MIN(d.path) AS path FROM note_metadata m JOIN documents d ON d.id = m.document_id
                WHERE m.status = 'canon' AND m.note_type = ? AND (? IS NULL OR m.role = ?) AND (? IS NULL OR m.character_status = ?)
                GROUP BY m.note_id ORDER BY m.note_id LIMIT ?")
                .bind(note_type).bind(role).bind(role).bind(status).bind(status).bind(i64::try_from(LIST_LIMIT)?)
                .fetch_all(&mut *tx).await?;
            for row in rows {
                let path: String = row.get("path");
                notes.push(StructuredNote {
                    id: row.get("note_id"),
                    title: std::path::Path::new(&path)
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                });
            }
        }
        tx.commit().await?;
        Ok(StructuredResult { total, notes })
    }

    pub async fn search_lexical(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
        let expression = lexical_expression(query);
        if expression.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            "SELECT d.path, c.chunk_index, c.heading, c.text, c.overlaps_previous
            FROM chunk_fts JOIN chunks c ON c.id = chunk_fts.rowid
            JOIN documents d ON d.id = c.document_id
            WHERE chunk_fts MATCH ? ORDER BY bm25(chunk_fts, 2.0, 1.0), c.id LIMIT ?",
        )
        .bind(expression)
        .bind(i64::try_from(limit)?)
        .fetch_all(&self.pool)
        .await
        .context("Failed to search FTS5")?;
        Ok(rows
            .into_iter()
            .map(|row| SearchResult {
                document_path: row.get("path"),
                chunk_index: row.get("chunk_index"),
                heading: row.get("heading"),
                text: row.get("text"),
                overlaps_previous: row.get("overlaps_previous"),
                distance: f32::INFINITY,
            })
            .collect())
    }

    pub async fn search_similar(
        &self,
        embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        if embedding.len() != crate::chronicle::indexer::embedder::EMBEDDING_DIMENSIONS {
            anyhow::bail!(
                "Expected embedding dimension {}, got {}",
                crate::chronicle::indexer::embedder::EMBEDDING_DIMENSIONS,
                embedding.len()
            );
        }

        if limit == 0 {
            return Ok(Vec::new());
        }

        let embedding_json =
            serde_json::to_string(embedding).context("Failed to serialise query embedding")?;

        let rows = sqlx::query(
            r"
            SELECT
                d.path,
                c.chunk_index,
                c.heading,
                c.text,
                c.overlaps_previous,
                ce.distance
            FROM chunk_embeddings ce
            JOIN chunks c ON c.id = ce.rowid
            JOIN documents d ON d.id = c.document_id
            WHERE ce.embedding MATCH ?
            AND k = ?
            ORDER BY ce.distance
            ",
        )
        .bind(embedding_json)
        .bind(i64::try_from(limit).context("Search result limit does not fit in SQLite integer")?)
        .fetch_all(&self.pool)
        .await
        .context("Failed to perform vector similarity search")?;

        Ok(rows
            .into_iter()
            .map(|row| SearchResult {
                document_path: row.get("path"),
                chunk_index: row.get("chunk_index"),
                heading: row.get("heading"),
                text: row.get("text"),
                overlaps_previous: row.get("overlaps_previous"),
                distance: row.get("distance"),
            })
            .collect())
    }
}

#[allow(clippy::too_many_lines)]
async fn write_metadata(
    connection: &mut sqlx::SqliteConnection,
    document_id: i64,
    metadata: &crate::chronicle::indexer::frontmatter::Metadata,
) -> Result<()> {
    sqlx::query("INSERT OR REPLACE INTO note_metadata(document_id, note_id, note_type, status, visibility, aliases, tags, summary, created, updated, role, character_status) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
        .bind(document_id).bind(&metadata.id).bind(&metadata.note_type)
        .bind(&metadata.status).bind(&metadata.visibility)
        .bind(serde_json::to_string(&metadata.aliases)?).bind(serde_json::to_string(&metadata.tags)?)
        .bind(&metadata.summary).bind(&metadata.created).bind(&metadata.updated)
        .bind(string_field(metadata, "role").or_else(|| metadata.role.map(crate::chronicle::query::plan::CharacterRole::as_str)))
        .bind(string_field(metadata, "character_status").or_else(|| metadata.character_status.map(crate::chronicle::query::plan::CharacterStatus::as_str)))
        .execute(&mut *connection).await?;

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
    ] {
        sqlx::query(&format!("DELETE FROM {table} WHERE document_id = ?"))
            .bind(document_id)
            .execute(&mut *connection)
            .await?;
    }
    sqlx::query("DELETE FROM note_wikilinks WHERE document_id = ?")
        .bind(document_id)
        .execute(&mut *connection)
        .await?;
    sqlx::query("DELETE FROM note_string_lists WHERE document_id = ?")
        .bind(document_id)
        .execute(&mut *connection)
        .await?;

    for (field_name, value) in &metadata.fields {
        match value {
            crate::chronicle::indexer::frontmatter::MetadataValue::WikilinkList(values) => {
                for (position, value) in values.iter().enumerate() {
                    sqlx::query("INSERT INTO note_wikilinks(document_id, field_name, position, value) VALUES (?, ?, ?, ?)")
                        .bind(document_id).bind(field_name).bind(i64::try_from(position)?).bind(value)
                        .execute(&mut *connection).await?;
                }
            }
            crate::chronicle::indexer::frontmatter::MetadataValue::StringList(values) => {
                for (position, value) in values.iter().enumerate() {
                    sqlx::query("INSERT INTO note_string_lists(document_id, field_name, position, value) VALUES (?, ?, ?, ?)")
                        .bind(document_id).bind(field_name).bind(i64::try_from(position)?).bind(value)
                        .execute(&mut *connection).await?;
                }
            }
            _ => {}
        }
    }

    match metadata.note_type.as_str() {
        "adventure" => {
            sqlx::query("INSERT INTO adventure_metadata(document_id, adventure_status, start_date, end_date, system, part_of_adventure, level_range) VALUES (?, ?, ?, ?, ?, ?, ?)")
                .bind(document_id).bind(string_field(metadata, "adventure_status"))
                .bind(string_field(metadata, "start_date")).bind(string_field(metadata, "end_date"))
                .bind(string_field(metadata, "system")).bind(string_field(metadata, "part_of_adventure"))
                .bind(string_field(metadata, "level_range")).execute(&mut *connection).await?;
        }
        "aspect" => {
            sqlx::query("INSERT INTO aspect_metadata(document_id) VALUES (?)")
                .bind(document_id)
                .execute(&mut *connection)
                .await?;
        }
        "character" => {
            sqlx::query("INSERT INTO character_metadata(document_id, race, role, character_status, location, birthplace, nationality, played_by, pronouns) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)")
                .bind(document_id).bind(string_field(metadata, "race"))
                .bind(string_field(metadata, "role").or_else(|| metadata.role.map(crate::chronicle::query::plan::CharacterRole::as_str)))
                .bind(string_field(metadata, "character_status").or_else(|| metadata.character_status.map(crate::chronicle::query::plan::CharacterStatus::as_str)))
                .bind(string_field(metadata, "location")).bind(string_field(metadata, "birthplace"))
                .bind(string_field(metadata, "nationality")).bind(string_field(metadata, "played_by"))
                .bind(string_field(metadata, "pronouns")).execute(&mut *connection).await?;
        }
        "deity" => {
            sqlx::query("INSERT INTO deity_metadata(document_id, pantheon, domain, antidomain, alignment, form, crystal) VALUES (?, ?, ?, ?, ?, ?, ?)")
                .bind(document_id).bind(string_field(metadata, "pantheon"))
                .bind(string_field(metadata, "domain")).bind(string_field(metadata, "antidomain"))
                .bind(string_field(metadata, "alignment")).bind(string_field(metadata, "form"))
                .bind(string_field(metadata, "crystal")).execute(&mut *connection).await?;
        }
        "event" => {
            sqlx::query("INSERT INTO event_metadata(document_id, event_type, occurred, occurred_start, occurred_end, historicity, result) VALUES (?, ?, ?, ?, ?, ?, ?)")
                .bind(document_id).bind(string_field(metadata, "event_type"))
                .bind(string_field(metadata, "occurred")).bind(string_field(metadata, "occurred_start"))
                .bind(string_field(metadata, "occurred_end")).bind(string_field(metadata, "historicity"))
                .bind(string_field(metadata, "result")).execute(&mut *connection).await?;
        }
        "language" => {
            sqlx::query("INSERT INTO language_metadata(document_id) VALUES (?)")
                .bind(document_id)
                .execute(&mut *connection)
                .await?;
        }
        "location" => {
            sqlx::query("INSERT INTO location_metadata(document_id, location_type, contained_in, population, demonym) VALUES (?, ?, ?, ?, ?)")
                .bind(document_id).bind(string_field(metadata, "location_type"))
                .bind(string_field(metadata, "contained_in")).bind(string_field(metadata, "population"))
                .bind(string_field(metadata, "demonym")).execute(&mut *connection).await?;
        }
        "lore" => {
            sqlx::query("INSERT INTO lore_metadata(document_id, lore_type, common_knowledge) VALUES (?, ?, ?)")
                .bind(document_id).bind(string_field(metadata, "lore_type"))
                .bind(bool_field(metadata, "common_knowledge")).execute(&mut *connection).await?;
        }
        "metagame" => {
            sqlx::query("INSERT INTO metagame_metadata(document_id, category, system, session_date) VALUES (?, ?, ?, ?)")
                .bind(document_id).bind(string_field(metadata, "category"))
                .bind(string_field(metadata, "system")).bind(string_field(metadata, "session_date"))
                .execute(&mut *connection).await?;
        }
        "monster" => {
            sqlx::query("INSERT INTO monster_metadata(document_id, creature_type, threat_level, alignment, size, source_inspiration) VALUES (?, ?, ?, ?, ?, ?)")
                .bind(document_id).bind(string_field(metadata, "creature_type"))
                .bind(string_field(metadata, "threat_level")).bind(string_field(metadata, "alignment"))
                .bind(string_field(metadata, "size")).bind(string_field(metadata, "source_inspiration"))
                .execute(&mut *connection).await?;
        }
        "object" => {
            sqlx::query("INSERT INTO object_metadata(document_id, object_type, rarity, owner, location, creator, attunement) VALUES (?, ?, ?, ?, ?, ?, ?)")
                .bind(document_id).bind(string_field(metadata, "object_type"))
                .bind(string_field(metadata, "rarity")).bind(string_field(metadata, "owner"))
                .bind(string_field(metadata, "location")).bind(string_field(metadata, "creator"))
                .bind(string_field(metadata, "attunement")).execute(&mut *connection).await?;
        }
        "organisation" => {
            sqlx::query("INSERT INTO organisation_metadata(document_id, organisation_type, leader, founder, headquarters, founded, dissolved, motto) VALUES (?, ?, ?, ?, ?, ?, ?, ?)")
                .bind(document_id).bind(string_field(metadata, "organisation_type"))
                .bind(string_field(metadata, "leader")).bind(string_field(metadata, "founder"))
                .bind(string_field(metadata, "headquarters"))
                .bind(string_field(metadata, "founded")).bind(string_field(metadata, "dissolved"))
                .bind(string_field(metadata, "motto")).execute(&mut *connection).await?;
        }
        "race" => {
            sqlx::query(
                "INSERT INTO race_metadata(document_id, lifespan, playable) VALUES (?, ?, ?)",
            )
            .bind(document_id)
            .bind(string_field(metadata, "lifespan"))
            .bind(bool_field(metadata, "playable"))
            .execute(&mut *connection)
            .await?;
        }
        _ => {}
    }
    Ok(())
}

fn string_field<'a>(
    metadata: &'a crate::chronicle::indexer::frontmatter::Metadata,
    name: &str,
) -> Option<&'a str> {
    match metadata.fields.get(name) {
        Some(
            crate::chronicle::indexer::frontmatter::MetadataValue::String(value)
            | crate::chronicle::indexer::frontmatter::MetadataValue::Date(value)
            | crate::chronicle::indexer::frontmatter::MetadataValue::FantasyDate(value)
            | crate::chronicle::indexer::frontmatter::MetadataValue::Wikilink(value)
            | crate::chronicle::indexer::frontmatter::MetadataValue::Enum(value),
        ) => Some(value),
        _ => None,
    }
}

fn bool_field(
    metadata: &crate::chronicle::indexer::frontmatter::Metadata,
    name: &str,
) -> Option<bool> {
    match metadata.fields.get(name) {
        Some(crate::chronicle::indexer::frontmatter::MetadataValue::Boolean(value)) => Some(*value),
        _ => None,
    }
}

/// Quote literal words so user input cannot become FTS query syntax.
fn lexical_expression(query: &str) -> String {
    query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .take(128)
        .map(|word| format!("\"{word}\""))
        .collect::<Vec<_>>()
        .join(" OR ")
}

pub(super) fn register_sqlite_vec() {
    unsafe {
        libsqlite3_sys::sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            unsafe extern "C" fn(
                *mut libsqlite3_sys::sqlite3,
                *mut *mut i8,
                *const libsqlite3_sys::sqlite3_api_routines,
            ) -> i32,
        >(
            sqlite_vec::sqlite3_vec_init as *const ()
        )));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn embedding(value: f32) -> Vec<f32> {
        vec![value; crate::chronicle::indexer::embedder::EMBEDDING_DIMENSIONS]
    }

    fn chunks() -> Vec<IndexedChunk> {
        vec![
            IndexedChunk {
                chunk_index: 0,
                heading: Some("Introduction".into()),
                text: "First chunk".into(),
                overlaps_previous: false,
            },
            IndexedChunk {
                chunk_index: 1,
                heading: Some("Introduction".into()),
                text: "Second chunk".into(),
                overlaps_previous: true,
            },
        ]
    }

    async fn test_database() -> anyhow::Result<(tempfile::TempDir, IndexerDb)> {
        let directory = tempdir()?;
        let url = format!(
            "sqlite://{}",
            directory.path().join("chronicle.db").display()
        );
        Ok((directory, IndexerDb::open(&url).await?))
    }

    #[tokio::test]
    async fn structured_lists_are_capped_but_counts_are_distinct_and_complete() -> Result<()> {
        let (_directory, db) = test_database().await?;
        let (mut metadata, _) = crate::chronicle::indexer::frontmatter::parse("---\nid: initial\ntype: character\nstatus: canon\nvisibility: player\ncreated: 2026-09-07\nupdated: 2026-09-07\nrole: npc\n---\n")?.context("note")?;
        for i in 0..25 {
            metadata.id = format!("id-{i:02}");
            db.replace_note(&format!("Note {i}.md"), "hash", &[], &[], &metadata)
                .await?;
        }
        db.replace_note("Duplicate.md", "hash", &[], &[], &metadata)
            .await?;
        let plan = crate::chronicle::query::planner::parse(
            r#"{"operation":"list","note_type":"character","filters":{"role":"npc"}}"#,
        )?;
        let result = db.execute_plan(&plan).await?;
        assert_eq!(result.total, 25);
        assert_eq!(result.notes.len(), 20);
        assert_eq!(result.notes[0].id, "id-00");
        assert_eq!(result.notes[19].id, "id-19");
        Ok(())
    }

    #[tokio::test]
    async fn type_specific_metadata_round_trips_and_replaces_lists() -> Result<()> {
        let (_directory, db) = test_database().await?;
        let source = "---\nid: ember-guild\ntype: organisation\nstatus: canon\nvisibility: player\ncreated: 2026-09-07\nupdated: 2026-09-07\norganisation_type: guild\nleader: '[[Tovan]]'\npatron_deity: ['[[Aurelia]]', '[[Veyra]]']\nideology: [craft, mutual-aid]\n---\n";
        let (metadata, _) =
            crate::chronicle::indexer::frontmatter::parse(source)?.context("note")?;
        let document_id = db
            .replace_note("Ember Guild.md", "hash", &[], &[], &metadata)
            .await?;

        let row = sqlx::query("SELECT organisation_type, leader, motto FROM organisation_metadata WHERE document_id = ?")
            .bind(document_id).fetch_one(&db.pool).await?;
        assert_eq!(row.get::<String, _>("organisation_type"), "guild");
        assert_eq!(row.get::<String, _>("leader"), "[[Tovan]]");
        assert!(row.get::<Option<String>, _>("motto").is_none());

        let links = sqlx::query("SELECT field_name, position, value FROM note_wikilinks WHERE document_id = ? ORDER BY field_name, position")
            .bind(document_id).fetch_all(&db.pool).await?;
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].get::<String, _>("field_name"), "patron_deity");
        assert_eq!(links[0].get::<i64, _>("position"), 0);
        assert_eq!(links[0].get::<String, _>("value"), "[[Aurelia]]");
        assert_eq!(links[1].get::<String, _>("value"), "[[Veyra]]");

        let strings = sqlx::query("SELECT field_name, position, value FROM note_string_lists WHERE document_id = ? ORDER BY position")
            .bind(document_id).fetch_all(&db.pool).await?;
        assert_eq!(strings.len(), 2);
        assert_eq!(strings[0].get::<String, _>("value"), "craft");
        assert_eq!(strings[1].get::<String, _>("value"), "mutual-aid");

        let replacement = source.replace("'[[Aurelia]]', '[[Veyra]]'", "'[[Veyra]]'");
        let (metadata, _) =
            crate::chronicle::indexer::frontmatter::parse(&replacement)?.context("note")?;
        db.replace_note("Ember Guild.md", "hash-2", &[], &[], &metadata)
            .await?;
        let links =
            sqlx::query("SELECT value FROM note_wikilinks WHERE document_id = ? ORDER BY position")
                .bind(document_id)
                .fetch_all(&db.pool)
                .await?;
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].get::<String, _>("value"), "[[Veyra]]");
        Ok(())
    }

    #[tokio::test]
    async fn replacing_a_note_type_removes_the_old_type_metadata() -> Result<()> {
        let (_directory, db) = test_database().await?;
        let character = "---\nid: shifting-note\ntype: character\nstatus: canon\nvisibility: player\ncreated: 2026-09-07\nupdated: 2026-09-07\nrole: npc\ncharacter_status: alive\nlocation: '[[Northmere]]'\n---\n";
        let (metadata, _) =
            crate::chronicle::indexer::frontmatter::parse(character)?.context("character")?;
        let document_id = db
            .replace_note("Shifting.md", "character", &[], &[], &metadata)
            .await?;
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM character_metadata WHERE document_id = ?"
            )
            .bind(document_id)
            .fetch_one(&db.pool)
            .await?,
            1
        );

        let organisation = character
            .replace("type: character", "type: organisation")
            .replace(
                "role: npc\ncharacter_status: alive\nlocation: '[[Northmere]]'",
                "organisation_type: guild\npatron_deity: ['[[Aurelia]]']",
            );
        let (metadata, _) = crate::chronicle::indexer::frontmatter::parse(&organisation)?
            .context("organisation")?;
        db.replace_note("Shifting.md", "organisation", &[], &[], &metadata)
            .await?;
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM character_metadata WHERE document_id = ?"
            )
            .bind(document_id)
            .fetch_one(&db.pool)
            .await?,
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM organisation_metadata WHERE document_id = ?"
            )
            .bind(document_id)
            .fetch_one(&db.pool)
            .await?,
            1
        );
        assert_eq!(sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM note_wikilinks WHERE document_id = ? AND field_name = 'patron_deity'").bind(document_id).fetch_one(&db.pool).await?, 1);
        Ok(())
    }

    #[tokio::test]
    async fn lexical_index_tracks_replacements_deletions_and_reopen() -> Result<()> {
        let (directory, database) = test_database().await?;
        let id = database
            .replace_document(
                "guide.md",
                "a",
                &chunks(),
                &[embedding(0.0), embedding(1.0)],
            )
            .await?;
        assert_eq!(database.search_lexical("First", 10).await?.len(), 1);
        assert_eq!(database.search_lexical("Introduction", 10).await?.len(), 2);
        assert!(database.search_lexical("\" * : ()", 10).await?.is_empty());
        let replacement = vec![IndexedChunk {
            text: "Moonspire sanctuary".into(),
            ..chunks().remove(0)
        }];
        database
            .replace_document("guide.md", "b", &replacement, &[embedding(0.0)])
            .await?;
        assert!(database.search_lexical("First", 10).await?.is_empty());
        assert_eq!(
            database
                .search_lexical("Where is Moonspire?", 10)
                .await?
                .len(),
            1
        );
        let reopened = IndexerDb::open(&format!(
            "sqlite://{}",
            directory.path().join("chronicle.db").display()
        ))
        .await?;
        assert_eq!(reopened.search_lexical("Moonspire", 10).await?.len(), 1);
        database.delete_document(id).await?;
        assert!(reopened.search_lexical("Moonspire", 10).await?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn replace_document_rejects_mismatched_inputs_without_writing() -> anyhow::Result<()> {
        let (_directory, database) = test_database().await?;

        let Err(error) = database
            .replace_document("guide.md", "hash", &chunks(), &[embedding(0.0)])
            .await
        else {
            anyhow::bail!("mismatched chunks and embeddings should fail");
        };

        assert!(error.to_string().contains("Chunk/embedding count mismatch"));
        assert!(database.all_documents().await?.is_empty());
        assert!(!database.has_chunks().await?);
        Ok(())
    }

    #[tokio::test]
    async fn replacement_keeps_document_identity_and_removes_stale_chunks() -> anyhow::Result<()> {
        let (_directory, database) = test_database().await?;
        let document_id = database
            .replace_document(
                "guide.md",
                "first-hash",
                &chunks(),
                &[embedding(0.0), embedding(1.0)],
            )
            .await?;

        let replacement = vec![IndexedChunk {
            chunk_index: 0,
            heading: None,
            text: "Replacement chunk".into(),
            overlaps_previous: false,
        }];
        let replacement_id = database
            .replace_document("guide.md", "second-hash", &replacement, &[embedding(2.0)])
            .await?;

        assert_eq!(replacement_id, document_id);
        assert_eq!(
            database
                .all_documents()
                .await?
                .into_iter()
                .map(|document| (document.path, document.content_hash))
                .collect::<Vec<_>>(),
            vec![("guide.md".into(), "second-hash".into())]
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM chunks")
                .fetch_one(&database.pool)
                .await?,
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT text FROM chunks")
                .fetch_one(&database.pool)
                .await?,
            "Replacement chunk"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM chunk_embeddings")
                .fetch_one(&database.pool)
                .await?,
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn delete_document_removes_chunks_embeddings_and_corpus_state() -> anyhow::Result<()> {
        let (_directory, database) = test_database().await?;
        let document_id = database
            .replace_document(
                "guide.md",
                "hash",
                &chunks(),
                &[embedding(0.0), embedding(1.0)],
            )
            .await?;
        assert!(database.has_chunks().await?);

        database.delete_document(document_id).await?;

        assert!(database.all_documents().await?.is_empty());
        assert!(!database.has_chunks().await?);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM chunk_embeddings")
                .fetch_one(&database.pool)
                .await?,
            0
        );
        Ok(())
    }

    #[tokio::test]
    async fn search_rejects_wrong_dimension_and_short_circuits_zero_limit() -> anyhow::Result<()> {
        let (_directory, database) = test_database().await?;

        assert!(database.search_similar(&[0.0], 1).await.is_err());
        assert!(
            database
                .search_similar(&embedding(0.0), 0)
                .await?
                .is_empty()
        );
        Ok(())
    }
}
