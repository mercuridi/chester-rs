// Document indexing, replacement, synchronization, and metadata persistence.
use std::path::Path;

use anyhow::{Context, Result};
use chrono::Utc;
use sqlx::Row;

use super::{IndexedChunk, IndexedDocument, IndexerDb};

impl IndexerDb {
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
            &crate::chronicle::indexer::Metadata::default(),
        )
        .await
    }

    pub async fn replace_note(
        &self,
        path: &str,
        content_hash: &str,
        chunks: &[IndexedChunk],
        embeddings: &[Vec<f32>],
        metadata: &crate::chronicle::indexer::Metadata,
    ) -> Result<i64> {
        if chunks.len() != embeddings.len() {
            anyhow::bail!(
                "Chunk/embedding count mismatch: {} chunks, {} embeddings",
                chunks.len(),
                embeddings.len()
            );
        }

        let mut tx = self.pool.begin().await?;

        let document_id = upsert_document(&mut tx, path, content_hash, metadata).await?;
        delete_document_chunks(&mut tx, document_id).await?;
        insert_chunks(&mut tx, document_id, chunks, embeddings).await?;

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
        metadata: &crate::chronicle::indexer::Metadata,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        write_metadata(&mut tx, document_id, metadata).await?;
        sqlx::query("UPDATE documents SET content_hash = ? WHERE id = ?")
            .bind(fingerprint)
            .bind(document_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE documents SET metadata_hash = ? WHERE id = ?")
            .bind(metadata_hash(metadata))
            .bind(document_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn metadata_matches(
        &self,
        document_id: i64,
        metadata: &crate::chronicle::indexer::Metadata,
    ) -> Result<bool> {
        let stored: String = sqlx::query_scalar("SELECT metadata_hash FROM documents WHERE id = ?")
            .bind(document_id)
            .fetch_one(&self.pool)
            .await?;
        Ok(!stored.is_empty() && stored == metadata_hash(metadata))
    }

    pub async fn chunks_match(
        &self,
        document_id: i64,
        chunks: &[crate::chronicle::indexer::Chunk],
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
}

async fn upsert_document(
    connection: &mut sqlx::SqliteConnection,
    path: &str,
    content_hash: &str,
    metadata: &crate::chronicle::indexer::Metadata,
) -> Result<i64> {
    sqlx::query_scalar(
        r"
        INSERT INTO documents (path, content_hash, metadata_hash, indexed_at)
        VALUES (?, ?, ?, ?)
        ON CONFLICT(path) DO UPDATE SET
            content_hash = excluded.content_hash,
            metadata_hash = excluded.metadata_hash,
            indexed_at = excluded.indexed_at
        RETURNING id
        ",
    )
    .bind(path)
    .bind(content_hash)
    .bind(metadata_hash(metadata))
    .bind(Utc::now().to_rfc3339())
    .fetch_one(&mut *connection)
    .await
    .context("Failed to upsert indexed document")
}

fn metadata_hash(metadata: &crate::chronicle::indexer::Metadata) -> String {
    // Metadata uses ordered maps, making its Debug representation deterministic.
    // This is an internal cache key, not a persisted interchange format.
    format!("{metadata:?}")
}

async fn delete_document_chunks(
    connection: &mut sqlx::SqliteConnection,
    document_id: i64,
) -> Result<()> {
    sqlx::query(
        r"
        DELETE FROM chunk_embeddings_player
        WHERE rowid IN (SELECT id FROM chunks WHERE document_id = ?)
        ",
    )
    .bind(document_id)
    .execute(&mut *connection)
    .await
    .context("Failed to delete existing player chunk embeddings")?;

    sqlx::query(
        r"
        DELETE FROM chunk_embeddings_secret
        WHERE rowid IN (SELECT id FROM chunks WHERE document_id = ?)
        ",
    )
    .bind(document_id)
    .execute(&mut *connection)
    .await
    .context("Failed to delete existing secret chunk embeddings")?;

    sqlx::query("DELETE FROM chunks WHERE document_id = ?")
        .bind(document_id)
        .execute(&mut *connection)
        .await
        .context("Failed to delete existing chunks")?;

    Ok(())
}

async fn insert_chunks(
    connection: &mut sqlx::SqliteConnection,
    document_id: i64,
    chunks: &[IndexedChunk],
    embeddings: &[Vec<f32>],
) -> Result<()> {
    for (chunk, embedding) in chunks.iter().zip(embeddings) {
        let chunk_id: i64 = sqlx::query_scalar(
            r"
            INSERT INTO chunks (
                document_id, chunk_index, heading, text, visibility, overlaps_previous
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
        .fetch_one(&mut *connection)
        .await
        .context("Failed to insert chunk")?;

        let embedding_json =
            serde_json::to_string(embedding).context("Failed to serialise embedding")?;
        let embedding_table = match chunk.visibility {
            crate::chronicle::indexer::ChunkVisibility::Player => "chunk_embeddings_player",
            crate::chronicle::indexer::ChunkVisibility::Secret => "chunk_embeddings_secret",
        };
        sqlx::query(&format!(
            "INSERT INTO {embedding_table} (rowid, embedding) VALUES (?, ?)"
        ))
        .bind(chunk_id)
        .bind(embedding_json)
        .execute(&mut *connection)
        .await
        .context("Failed to insert chunk embedding")?;
    }

    Ok(())
}

async fn write_metadata(
    connection: &mut sqlx::SqliteConnection,
    document_id: i64,
    metadata: &crate::chronicle::indexer::Metadata,
) -> Result<()> {
    write_note_metadata(connection, document_id, metadata).await?;
    clear_metadata(connection, document_id).await?;
    write_identifiers(connection, document_id, metadata).await?;
    write_field_indexes(connection, document_id, metadata).await?;
    write_type_metadata(connection, document_id, metadata).await
}

async fn write_note_metadata(
    connection: &mut sqlx::SqliteConnection,
    document_id: i64,
    metadata: &crate::chronicle::indexer::Metadata,
) -> Result<()> {
    sqlx::query("INSERT OR REPLACE INTO note_metadata(document_id, note_id, note_type, status, visibility, aliases, tags, summary, created, updated) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
        .bind(document_id).bind(&metadata.id).bind(&metadata.note_type)
        .bind(&metadata.status).bind(&metadata.visibility)
        .bind(serde_json::to_string(&metadata.aliases)?).bind(serde_json::to_string(&metadata.tags)?)
        .bind(&metadata.summary).bind(&metadata.created).bind(&metadata.updated)
        .execute(&mut *connection).await?;

    Ok(())
}

async fn clear_metadata(connection: &mut sqlx::SqliteConnection, document_id: i64) -> Result<()> {
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
    sqlx::query("DELETE FROM note_identifiers WHERE document_id = ?")
        .bind(document_id)
        .execute(&mut *connection)
        .await?;

    Ok(())
}

async fn write_identifiers(
    connection: &mut sqlx::SqliteConnection,
    document_id: i64,
    metadata: &crate::chronicle::indexer::Metadata,
) -> Result<()> {
    let path: String = sqlx::query_scalar("SELECT path FROM documents WHERE id = ?")
        .bind(document_id)
        .fetch_one(&mut *connection)
        .await?;
    let mut identifiers = std::collections::BTreeSet::new();
    if !metadata.id.trim().is_empty() {
        identifiers.insert(metadata.id.trim().to_owned());
    }
    if let Some(title) = Path::new(&path)
        .file_stem()
        .and_then(|title| title.to_str())
        && !title.trim().is_empty()
    {
        identifiers.insert(title.trim().to_owned());
    }
    identifiers.extend(
        metadata
            .aliases
            .iter()
            .map(|alias| alias.trim())
            .filter(|alias| !alias.is_empty())
            .map(ToOwned::to_owned),
    );
    for identifier in identifiers {
        sqlx::query("INSERT INTO note_identifiers(document_id, value) VALUES (?, ?)")
            .bind(document_id)
            .bind(identifier)
            .execute(&mut *connection)
            .await?;
    }

    Ok(())
}

async fn write_field_indexes(
    connection: &mut sqlx::SqliteConnection,
    document_id: i64,
    metadata: &crate::chronicle::indexer::Metadata,
) -> Result<()> {
    for (field_name, value) in &metadata.fields {
        match value {
            crate::chronicle::indexer::MetadataValue::WikilinkList(values) => {
                for (position, value) in values.iter().enumerate() {
                    sqlx::query("INSERT INTO note_wikilinks(document_id, field_name, position, value) VALUES (?, ?, ?, ?)")
                        .bind(document_id).bind(field_name).bind(i64::try_from(position)?).bind(value)
                        .execute(&mut *connection).await?;
                }
            }
            crate::chronicle::indexer::MetadataValue::StringList(values) => {
                for (position, value) in values.iter().enumerate() {
                    sqlx::query("INSERT INTO note_string_lists(document_id, field_name, position, value) VALUES (?, ?, ?, ?)")
                        .bind(document_id).bind(field_name).bind(i64::try_from(position)?).bind(value)
                    .execute(&mut *connection).await?;
                }
            }
            crate::chronicle::indexer::MetadataValue::String(value)
            | crate::chronicle::indexer::MetadataValue::Date(value)
            | crate::chronicle::indexer::MetadataValue::FantasyDate(value)
            | crate::chronicle::indexer::MetadataValue::Wikilink(value)
            | crate::chronicle::indexer::MetadataValue::StringOrWikilink(value)
            | crate::chronicle::indexer::MetadataValue::Enum(value) => {
                sqlx::query("INSERT INTO note_scalar_fields(document_id, field_name, value) VALUES (?, ?, ?)")
                    .bind(document_id)
                    .bind(field_name)
                    .bind(value)
                    .execute(&mut *connection)
                    .await?;
            }
            crate::chronicle::indexer::MetadataValue::Boolean(value) => {
                sqlx::query("INSERT INTO note_scalar_fields(document_id, field_name, value) VALUES (?, ?, ?)")
                    .bind(document_id)
                    .bind(field_name)
                    .bind(value.to_string())
                    .execute(&mut *connection)
                    .await?;
            }
        }
    }

    Ok(())
}

// This is intentionally explicit: it is the schema-to-table persistence map.
async fn write_type_metadata(
    connection: &mut sqlx::SqliteConnection,
    document_id: i64,
    metadata: &crate::chronicle::indexer::Metadata,
) -> Result<()> {
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
            sqlx::query("INSERT INTO character_metadata(document_id, race, life_status_cause, life_status_since, location, birthplace, birth_year, nationality, played_by, pronouns) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
                .bind(document_id).bind(string_field(metadata, "race"))
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
    metadata: &'a crate::chronicle::indexer::Metadata,
    name: &str,
) -> Option<&'a str> {
    match metadata.fields.get(name) {
        Some(
            crate::chronicle::indexer::MetadataValue::String(value)
            | crate::chronicle::indexer::MetadataValue::Date(value)
            | crate::chronicle::indexer::MetadataValue::FantasyDate(value)
            | crate::chronicle::indexer::MetadataValue::Wikilink(value)
            | crate::chronicle::indexer::MetadataValue::Enum(value)
            | crate::chronicle::indexer::MetadataValue::StringOrWikilink(value),
        ) => Some(value),
        _ => None,
    }
}

fn bool_field(metadata: &crate::chronicle::indexer::Metadata, name: &str) -> Option<bool> {
    match metadata.fields.get(name) {
        Some(crate::chronicle::indexer::MetadataValue::Boolean(value)) => Some(*value),
        _ => None,
    }
}
