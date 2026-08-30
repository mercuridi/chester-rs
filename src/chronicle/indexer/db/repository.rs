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
