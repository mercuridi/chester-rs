// src/chronicle/indexer/db/repository.rs

use anyhow::{Context, Result};
use chrono::Utc;
use sqlx::{QueryBuilder, Row, Sqlite, sqlite::SqlitePool};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessScope {
    Player,
    Gm,
}

impl AccessScope {
    const fn is_gm(self) -> bool {
        matches!(self, Self::Gm)
    }
}

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
    pub visibility: crate::chronicle::indexer::document::ChunkVisibility,
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

        let database_url = versioned_database_url(path);
        let pool = crate::database::pool::open_sqlite_pool(&database_url, "Chronicle").await?;

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
            DELETE FROM chunk_embeddings_player
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
        .context("Failed to delete player document embeddings")?;

        sqlx::query(
            r"
            DELETE FROM chunk_embeddings_secret
            WHERE rowid IN (
                SELECT id FROM chunks WHERE document_id = ?
            )
            ",
        )
        .bind(document_id)
        .execute(&mut *tx)
        .await
        .context("Failed to delete secret document embeddings")?;

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
            DELETE FROM chunk_embeddings_player
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
        .context("Failed to delete existing player chunk embeddings")?;

        sqlx::query(
            r"
            DELETE FROM chunk_embeddings_secret
            WHERE rowid IN (
                SELECT id FROM chunks WHERE document_id = ?
            )
            ",
        )
        .bind(document_id)
        .execute(&mut *tx)
        .await
        .context("Failed to delete existing secret chunk embeddings")?;

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
                    visibility,
                    overlaps_previous
                )
                VALUES (?, ?, ?, ?, ?, ?)
                RETURNING id
                ",
            )
            .bind(document_id)
            .bind(chunk.chunk_index)
            .bind(&chunk.heading)
            .bind(&chunk.text)
            .bind(chunk.visibility.as_str())
            .bind(chunk.overlaps_previous)
            .fetch_one(&mut *tx)
            .await
            .context("Failed to insert chunk")?;

            let embedding_json =
                serde_json::to_string(embedding).context("Failed to serialise embedding")?;

            let embedding_table = match chunk.visibility {
                crate::chronicle::indexer::document::ChunkVisibility::Player => {
                    "chunk_embeddings_player"
                }
                crate::chronicle::indexer::document::ChunkVisibility::Secret => {
                    "chunk_embeddings_secret"
                }
            };
            sqlx::query(&format!(
                "INSERT INTO {embedding_table} (rowid, embedding) VALUES (?, ?)"
            ))
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
        let rows = sqlx::query("SELECT chunk_index, heading, text, visibility, overlaps_previous FROM chunks WHERE document_id = ? ORDER BY chunk_index")
            .bind(document_id).fetch_all(&self.pool).await?;
        Ok(rows.len() == chunks.len()
            && rows.iter().zip(chunks).all(|(row, chunk)| {
                usize::try_from(row.get::<i64, _>("chunk_index")).ok() == Some(chunk.index)
                    && row.get::<Option<String>, _>("heading") == chunk.heading
                    && row.get::<String, _>("text") == chunk.content
                    && row.get::<String, _>("visibility") == chunk.visibility.as_str()
                    && row.get::<bool, _>("overlaps_previous") == (chunk.overlap_tokens > 0)
            }))
    }

    pub async fn execute_plan(
        &self,
        plan: &crate::chronicle::query::plan::Plan,
    ) -> Result<StructuredResult> {
        self.execute_plan_for(plan, AccessScope::Gm).await
    }

    pub async fn execute_plan_for(
        &self,
        plan: &crate::chronicle::query::plan::Plan,
        access: AccessScope,
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
        let mut count = structured_query(
            "SELECT COUNT(DISTINCT m.note_id) FROM note_metadata m",
            note_type,
            filters,
            role,
            status,
            access,
        );
        let total: i64 = count.build_query_scalar().fetch_one(&self.pool).await?;
        let mut notes = Vec::new();
        if matches!(plan, Plan::List { .. }) {
            let mut query = structured_query(
                "SELECT m.note_id, MIN(d.path) AS path FROM note_metadata m JOIN documents d ON d.id = m.document_id",
                note_type,
                filters,
                role,
                status,
                access,
            );
            query
                .push(" GROUP BY m.note_id ORDER BY m.note_id LIMIT ")
                .push_bind(i64::try_from(LIST_LIMIT)?);
            let rows = query.build().fetch_all(&self.pool).await?;
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
        Ok(StructuredResult { total, notes })
    }

    pub async fn search_lexical(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
        self.search_lexical_for(query, limit, AccessScope::Gm).await
    }

    pub async fn search_lexical_for(
        &self,
        query: &str,
        limit: usize,
        access: AccessScope,
    ) -> Result<Vec<SearchResult>> {
        let expression = lexical_expression(query);
        if expression.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            "SELECT d.path, c.chunk_index, c.heading, c.text, c.overlaps_previous
            FROM chunk_fts JOIN chunks c ON c.id = chunk_fts.rowid
            JOIN documents d ON d.id = c.document_id
            WHERE chunk_fts MATCH ? AND (? OR c.visibility = 'player') ORDER BY bm25(chunk_fts, 2.0, 1.0), c.id LIMIT ?",
        )
        .bind(expression)
        .bind(access.is_gm())
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
        self.search_similar_for(embedding, limit, AccessScope::Gm)
            .await
    }

    pub async fn search_similar_for(
        &self,
        embedding: &[f32],
        limit: usize,
        access: AccessScope,
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

        let mut results = self
            .search_similar_in_table("chunk_embeddings_player", &embedding_json, limit)
            .await?;
        if access.is_gm() {
            results.extend(
                self.search_similar_in_table("chunk_embeddings_secret", &embedding_json, limit)
                    .await?,
            );
        }
        results.sort_by(|left, right| {
            left.distance
                .total_cmp(&right.distance)
                .then_with(|| left.document_path.cmp(&right.document_path))
                .then_with(|| left.chunk_index.cmp(&right.chunk_index))
        });
        results.truncate(limit);
        Ok(results)
    }

    async fn search_similar_in_table(
        &self,
        table: &str,
        embedding_json: &str,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        debug_assert!(matches!(
            table,
            "chunk_embeddings_player" | "chunk_embeddings_secret"
        ));
        let rows = sqlx::query(&format!(
            r"
            SELECT
                d.path,
                c.chunk_index,
                c.heading,
                c.text,
                c.overlaps_previous,
                ce.distance
            FROM {table} ce
            JOIN chunks c ON c.id = ce.rowid
            JOIN documents d ON d.id = c.document_id
            WHERE ce.embedding MATCH ?
            AND k = ?
            ORDER BY ce.distance
            "
        ))
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

fn structured_query<'a>(
    select: &str,
    note_type: &'a str,
    filters: &'a crate::chronicle::query::plan::Filters,
    role: Option<&'static str>,
    status: Option<&'static str>,
    access: AccessScope,
) -> QueryBuilder<'a, Sqlite> {
    use crate::chronicle::query::plan::ConditionOperator;

    let mut query = QueryBuilder::new(select);
    query
        .push(" WHERE m.status = 'canon' AND m.note_type = ")
        .push_bind(note_type);
    if access == AccessScope::Player {
        query.push(" AND m.visibility != 'secret'");
    }
    if let Some(role) = role {
        query.push(" AND m.role = ").push_bind(role);
    }
    if let Some(status) = status {
        query.push(" AND m.life_status = ").push_bind(status);
    }
    for condition in &filters.conditions {
        match condition.operator {
            ConditionOperator::Equals => {
                query
                    .push(" AND EXISTS (SELECT 1 FROM note_scalar_fields s WHERE s.document_id = m.document_id AND s.field_name = ")
                    .push_bind(&condition.field)
                    .push(" AND s.value = ")
                    .push_bind(&condition.value)
                    .push(")");
            }
            ConditionOperator::Contains => {
                let definition = crate::chronicle::indexer::schema::field_definition(
                    note_type,
                    &condition.field,
                )
                .expect("validated query condition field");
                let table = match definition.value_type {
                    crate::chronicle::indexer::schema::ValueType::WikilinkList => "note_wikilinks",
                    crate::chronicle::indexer::schema::ValueType::StringList => "note_string_lists",
                    _ => unreachable!("validated contains condition must be a list"),
                };
                query
                    .push(" AND EXISTS (SELECT 1 FROM ")
                    .push(table)
                    .push(" l WHERE l.document_id = m.document_id AND l.field_name = ")
                    .push_bind(&condition.field)
                    .push(" AND l.value = ")
                    .push_bind(&condition.value)
                    .push(")");
            }
        }
    }
    query
}

/// Returns the cache location for the current derived-index format.
///
/// The format is encoded in the filename rather than tracked inside `SQLite`:
/// bumping `INDEX_FORMAT_VERSION` therefore always selects a fresh database.
fn versioned_database_url(database_url: &str) -> String {
    let Some(path_and_query) = database_url.strip_prefix("sqlite://") else {
        return database_url.to_owned();
    };
    let (path, query) = path_and_query
        .split_once('?')
        .map_or((path_and_query, None), |(path, query)| (path, Some(query)));
    if path == ":memory:" {
        return database_url.to_owned();
    }

    let path = Path::new(path);
    let (Some(stem), Some(extension)) = (path.file_stem(), path.extension()) else {
        return database_url.to_owned();
    };
    let versioned_filename = format!(
        "{}.index-v{}.{}",
        stem.to_string_lossy(),
        super::schema::INDEX_FORMAT_VERSION,
        extension.to_string_lossy()
    );
    let versioned_path = path.with_file_name(versioned_filename);
    match query {
        Some(query) => format!("sqlite://{}?{query}", versioned_path.display()),
        None => format!("sqlite://{}", versioned_path.display()),
    }
}

#[allow(clippy::too_many_lines)]
async fn write_metadata(
    connection: &mut sqlx::SqliteConnection,
    document_id: i64,
    metadata: &crate::chronicle::indexer::frontmatter::Metadata,
) -> Result<()> {
    sqlx::query("INSERT OR REPLACE INTO note_metadata(document_id, note_id, note_type, status, visibility, aliases, tags, summary, created, updated, role, life_status) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
        .bind(document_id).bind(&metadata.id).bind(&metadata.note_type)
        .bind(&metadata.status).bind(&metadata.visibility)
        .bind(serde_json::to_string(&metadata.aliases)?).bind(serde_json::to_string(&metadata.tags)?)
        .bind(&metadata.summary).bind(&metadata.created).bind(&metadata.updated)
        .bind(string_field(metadata, "role").or_else(|| metadata.role.map(crate::chronicle::query::plan::CharacterRole::as_str)))
        .bind(string_field(metadata, "life_status").or_else(|| metadata.life_status.map(crate::chronicle::query::plan::CharacterStatus::as_str)))
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
    sqlx::query("DELETE FROM note_scalar_fields WHERE document_id = ?")
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
            crate::chronicle::indexer::frontmatter::MetadataValue::String(value)
            | crate::chronicle::indexer::frontmatter::MetadataValue::Date(value)
            | crate::chronicle::indexer::frontmatter::MetadataValue::FantasyDate(value)
            | crate::chronicle::indexer::frontmatter::MetadataValue::Wikilink(value)
            | crate::chronicle::indexer::frontmatter::MetadataValue::StringOrWikilink(value)
            | crate::chronicle::indexer::frontmatter::MetadataValue::Enum(value) => {
                sqlx::query("INSERT INTO note_scalar_fields(document_id, field_name, value) VALUES (?, ?, ?)")
                    .bind(document_id)
                    .bind(field_name)
                    .bind(value)
                    .execute(&mut *connection)
                    .await?;
            }
            crate::chronicle::indexer::frontmatter::MetadataValue::Boolean(value) => {
                sqlx::query("INSERT INTO note_scalar_fields(document_id, field_name, value) VALUES (?, ?, ?)")
                    .bind(document_id)
                    .bind(field_name)
                    .bind(value.to_string())
                    .execute(&mut *connection)
                    .await?;
            }
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
            sqlx::query("INSERT INTO character_metadata(document_id, race, role, life_status, life_status_cause, life_status_since, location, birthplace, birth_year, nationality, played_by, pronouns) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
                .bind(document_id).bind(string_field(metadata, "race"))
                .bind(string_field(metadata, "role").or_else(|| metadata.role.map(crate::chronicle::query::plan::CharacterRole::as_str)))
                .bind(string_field(metadata, "life_status").or_else(|| metadata.life_status.map(crate::chronicle::query::plan::CharacterStatus::as_str)))
                .bind(string_field(metadata, "life_status_cause")).bind(string_field(metadata, "life_status_since"))
                .bind(string_field(metadata, "location")).bind(string_field(metadata, "birthplace")).bind(string_field(metadata, "birth_year"))
                .bind(string_field(metadata, "nationality")).bind(string_field(metadata, "played_by"))
                .bind(string_field(metadata, "pronouns")).execute(&mut *connection).await?;
        }
        "deity" => {
            sqlx::query("INSERT INTO deity_metadata(document_id, deity_type, domain, antidomain, alignment, form, crystal) VALUES (?, ?, ?, ?, ?, ?, ?)")
                .bind(document_id).bind(string_field(metadata, "deity_type"))
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
            sqlx::query("INSERT INTO monster_metadata(document_id, creature_type, threat_level, alignment, source_inspiration) VALUES (?, ?, ?, ?, ?)")
                .bind(document_id).bind(string_field(metadata, "creature_type"))
                .bind(string_field(metadata, "threat_level")).bind(string_field(metadata, "alignment"))
                .bind(string_field(metadata, "source_inspiration"))
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
            | crate::chronicle::indexer::frontmatter::MetadataValue::Enum(value)
            | crate::chronicle::indexer::frontmatter::MetadataValue::StringOrWikilink(value),
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
                visibility: crate::chronicle::indexer::document::ChunkVisibility::Player,
                overlaps_previous: false,
            },
            IndexedChunk {
                chunk_index: 1,
                heading: Some("Introduction".into()),
                text: "Second chunk".into(),
                visibility: crate::chronicle::indexer::document::ChunkVisibility::Player,
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

    #[test]
    fn versioned_database_filename_selects_the_current_index_format() {
        assert_eq!(
            versioned_database_url("sqlite:///data/chronicle.sqlite3?mode=rwc"),
            format!(
                "sqlite:///data/chronicle.index-v{}.sqlite3?mode=rwc",
                super::super::schema::INDEX_FORMAT_VERSION
            )
        );
        assert_eq!(
            versioned_database_url("sqlite://:memory:"),
            "sqlite://:memory:"
        );
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
        let source = "---\nid: ember-guild\ntype: organisation\nstatus: canon\nvisibility: player\ncreated: 2026-09-07\nupdated: 2026-09-07\norganisation_type: guild\nleader: '[[Tovan]]'\npatron_deities: ['[[Aurelia]]', '[[Veyra]]']\nideology: [craft, mutual-aid]\n---\n";
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
        assert_eq!(links[0].get::<String, _>("field_name"), "patron_deities");
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
    async fn structured_conditions_query_scalar_and_wikilink_list_metadata() -> Result<()> {
        let (_directory, db) = test_database().await?;
        let source = "---\nid: tamsin\ntype: character\nstatus: canon\nvisibility: player\ncreated: 2026-09-07\nupdated: 2026-09-07\nlife_status: dead\nlife_status_cause: '[[Battle of Castle Vetra]]'\nappearances: ['[[Riftweavers]]']\n---\n";
        let (metadata, _) =
            crate::chronicle::indexer::frontmatter::parse(source)?.context("note")?;
        db.replace_note("Tamsin.md", "hash", &[], &[], &metadata)
            .await?;

        let plan = crate::chronicle::query::planner::parse(
            r#"{"operation":"list","note_type":"character","filters":{"conditions":[{"field":"life_status_cause","operator":"equals","value":"[[Battle of Castle Vetra]]"},{"field":"appearances","operator":"contains","value":"[[Riftweavers]]"}]}}"#,
        )?;
        let result = db.execute_plan(&plan).await?;
        assert_eq!(result.total, 1);
        assert_eq!(result.notes[0].id, "tamsin");
        Ok(())
    }

    #[tokio::test]
    async fn player_structured_queries_exclude_secret_notes() -> Result<()> {
        let (_directory, db) = test_database().await?;
        for (id, visibility) in [("public-npc", "player"), ("secret-npc", "secret")] {
            let source = format!(
                "---\nid: {id}\ntype: character\nstatus: canon\nvisibility: {visibility}\ncreated: 2026-09-07\nupdated: 2026-09-07\nrole: npc\n---\n"
            );
            let (metadata, _) =
                crate::chronicle::indexer::frontmatter::parse(&source)?.context("note")?;
            db.replace_note(&format!("{id}.md"), id, &[], &[], &metadata)
                .await?;
        }
        let plan = crate::chronicle::query::planner::parse(
            r#"{"operation":"count","note_type":"character","filters":{"role":"npc"}}"#,
        )?;
        assert_eq!(
            db.execute_plan_for(&plan, AccessScope::Player).await?.total,
            1
        );
        assert_eq!(db.execute_plan_for(&plan, AccessScope::Gm).await?.total, 2);
        Ok(())
    }

    #[tokio::test]
    async fn replacing_a_note_type_removes_the_old_type_metadata() -> Result<()> {
        let (_directory, db) = test_database().await?;
        let character = "---\nid: shifting-note\ntype: character\nstatus: canon\nvisibility: player\ncreated: 2026-09-07\nupdated: 2026-09-07\nrole: npc\nlife_status: alive\nlocation: '[[Northmere]]'\n---\n";
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
                "role: npc\nlife_status: alive\nlocation: '[[Northmere]]'",
                "organisation_type: guild\npatron_deities: ['[[Aurelia]]']",
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
        assert_eq!(sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM note_wikilinks WHERE document_id = ? AND field_name = 'patron_deities'").bind(document_id).fetch_one(&db.pool).await?, 1);
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
    async fn player_search_excludes_secret_chunks_while_gm_search_includes_them() -> Result<()> {
        let (_directory, database) = test_database().await?;
        let chunks = [IndexedChunk {
            chunk_index: 0,
            heading: Some("GM notes".into()),
            text: "moon-key-needle is hidden below the altar".into(),
            visibility: crate::chronicle::indexer::document::ChunkVisibility::Secret,
            overlaps_previous: false,
        }];
        database
            .replace_note(
                "mixed.md",
                "visibility-hash",
                &chunks,
                &[embedding(0.0)],
                &crate::chronicle::indexer::frontmatter::Metadata::default(),
            )
            .await?;

        assert!(
            database
                .search_lexical_for("moon-key-needle", 5, AccessScope::Player)
                .await?
                .is_empty()
        );
        assert_eq!(
            database
                .search_lexical_for("moon-key-needle", 5, AccessScope::Gm)
                .await?
                .len(),
            1
        );
        assert!(
            database
                .search_similar_for(&embedding(0.0), 5, AccessScope::Player)
                .await?
                .is_empty()
        );
        assert_eq!(
            database
                .search_similar_for(&embedding(0.0), 5, AccessScope::Gm)
                .await?
                .len(),
            1
        );
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
            visibility: crate::chronicle::indexer::document::ChunkVisibility::Player,
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
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM chunk_embeddings_player")
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
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM chunk_embeddings_player")
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
