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
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub document_path: String,
    #[expect(dead_code, reason = "Retained for future result display and reranking")]
    pub chunk_index: i64,
    pub heading: Option<String>,
    pub text: String,
    #[expect(dead_code, reason = "Retained for future result display and reranking")]
    pub distance: f32,
}

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

    pub async fn replace_document(
        &self,
        path: &str,
        content_hash: &str,
        chunks: &[IndexedChunk],
        embeddings: &[Vec<f32>],
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
                    text
                )
                VALUES (?, ?, ?, ?)
                RETURNING id
                ",
            )
            .bind(document_id)
            .bind(chunk.chunk_index)
            .bind(&chunk.heading)
            .bind(&chunk.text)
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

        tx.commit()
            .await
            .context("Failed to commit document replacement")?;

        Ok(document_id)
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
                distance: row.get("distance"),
            })
            .collect())
    }
}

fn register_sqlite_vec() {
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
