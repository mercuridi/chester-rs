// Full-text and vector candidate retrieval.
use anyhow::{Context, Result};
use sqlx::Row;

use super::facade::{AccessScope, IndexerDb, SearchResult};

impl IndexerDb {
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
