// Graph persistence and access-scoped PageRank signals.
use anyhow::{Context, Result, bail};
use sqlx::{QueryBuilder, Row, Sqlite};

use super::facade::{AccessScope, GraphStats, IndexerDb, PageRankSignal, PageRankStats};

const GRAPH_STATE_KEY: &str = "document-graph-input-v1";

impl IndexerDb {
    pub async fn graph_input_fingerprint(&self) -> Result<Option<String>> {
        sqlx::query_scalar("SELECT value FROM chronicle_index_state WHERE key = ?")
            .bind(GRAPH_STATE_KEY)
            .fetch_optional(&self.pool)
            .await
            .context("Failed to load Chronicle graph state")
    }

    pub async fn set_graph_input_fingerprint(&self, fingerprint: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO chronicle_index_state(key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(GRAPH_STATE_KEY)
        .bind(fingerprint)
        .execute(&self.pool)
        .await
        .context("Failed to persist Chronicle graph state")?;
        Ok(())
    }

    pub async fn pagerank_for_paths(
        &self,
        paths: impl IntoIterator<Item = String>,
        access: AccessScope,
    ) -> Result<std::collections::HashMap<String, PageRankSignal>> {
        let paths = paths.into_iter().collect::<std::collections::HashSet<_>>();
        if paths.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let mut query = QueryBuilder::<Sqlite>::new("SELECT d.path, CASE WHEN ");
        query
            .push_bind(access.is_gm())
            .push(" THEN p.gm_score ELSE p.player_score END AS score, CASE WHEN ")
            .push_bind(access.is_gm())
            .push(" THEN p.gm_rank ELSE p.player_rank END AS rank FROM documents d JOIN document_pagerank p ON p.document_id = d.id WHERE d.path IN (");
        let mut separated = query.separated(", ");
        for path in paths {
            separated.push_bind(path);
        }
        separated.push_unseparated(")");
        let rows = query
            .build()
            .fetch_all(&self.pool)
            .await
            .context("Failed to load PageRank signals for retrieval candidates")?;
        Ok(rows
            .into_iter()
            .map(|row| {
                (
                    row.get("path"),
                    PageRankSignal {
                        score: row.get("score"),
                        rank: row.get("rank"),
                    },
                )
            })
            .collect())
    }

    /// Replace the complete derived document graph with the resolver's current
    /// corpus-wide output. Rebuilding avoids stale edges after an identifier,
    /// alias, visibility, or source-link change.
    pub async fn rebuild_document_graph(
        &self,
        resolution: &crate::chronicle::indexer::link_resolver::LinkResolution,
    ) -> Result<GraphStats> {
        let mut tx = self.pool.begin().await?;
        let rows = sqlx::query("SELECT document_id, note_id FROM note_metadata")
            .fetch_all(&mut *tx)
            .await
            .context("Failed to load note IDs for graph persistence")?;
        let mut document_ids = std::collections::HashMap::new();
        for row in rows {
            let document_id: i64 = row.get("document_id");
            let note_id: String = row.get("note_id");
            if document_ids.insert(note_id.clone(), document_id).is_some() {
                bail!("Cannot build document graph: duplicate note ID `{note_id}`");
            }
        }

        sqlx::query("DELETE FROM document_graph_edges")
            .execute(&mut *tx)
            .await
            .context("Failed to clear existing document graph")?;

        let mut edge_count = 0_u64;
        for link in &resolution.resolved {
            let source_document_id = document_ids.get(&link.source_note_id).with_context(|| {
                format!(
                    "Resolved graph source `{}` is absent from the indexed corpus",
                    link.source_note_id
                )
            })?;
            let target_document_id = document_ids.get(&link.target_note_id).with_context(|| {
                format!(
                    "Resolved graph target `{}` is absent from the indexed corpus",
                    link.target_note_id
                )
            })?;
            let inserted = sqlx::query(
                "INSERT OR IGNORE INTO document_graph_edges (source_document_id, target_document_id, origin, field_name, visibility) VALUES (?, ?, ?, ?, ?)",
            )
            .bind(source_document_id)
            .bind(target_document_id)
            .bind(link.origin.kind())
            .bind(link.origin.field_name())
            .bind(link.visibility.as_str())
            .execute(&mut *tx)
            .await
            .context("Failed to persist resolved document graph edge")?;
            edge_count += inserted.rows_affected();
        }

        tx.commit()
            .await
            .context("Failed to commit resolved document graph")?;
        Ok(GraphStats { edge_count })
    }

    /// Recompute both access-scoped `PageRank` vectors from the persisted graph.
    /// Player scores use only player-visible nodes and edges, so secret graph
    /// topology cannot influence player retrieval ordering.
    pub async fn rebuild_document_pagerank(&self) -> Result<PageRankStats> {
        let mut tx = self.pool.begin().await?;
        let nodes =
            sqlx::query("SELECT document_id, visibility FROM note_metadata ORDER BY document_id")
                .fetch_all(&mut *tx)
                .await
                .context("Failed to load document graph nodes for PageRank")?;
        let all_nodes = nodes
            .iter()
            .map(|row| row.get::<i64, _>("document_id"))
            .collect::<Vec<_>>();
        let player_nodes = nodes
            .iter()
            .filter(|row| row.get::<String, _>("visibility") != "secret")
            .map(|row| row.get::<i64, _>("document_id"))
            .collect::<Vec<_>>();

        let gm_edges = graph_edges(&mut tx, false).await?;
        let player_edges = graph_edges(&mut tx, true).await?;
        let gm = crate::chronicle::indexer::pagerank::compute(&all_nodes, &gm_edges);
        let player = crate::chronicle::indexer::pagerank::compute(&player_nodes, &player_edges);
        let player_entries = player
            .entries
            .iter()
            .map(|entry| (entry.document_id, (entry.score, entry.rank)))
            .collect::<std::collections::HashMap<_, _>>();

        sqlx::query("DELETE FROM document_pagerank")
            .execute(&mut *tx)
            .await
            .context("Failed to clear existing PageRank scores")?;
        for entry in &gm.entries {
            let (player_score, player_rank) = player_entries
                .get(&entry.document_id)
                .copied()
                .unwrap_or((0.0, 0));
            sqlx::query(
                "INSERT INTO document_pagerank (document_id, player_score, player_rank, gm_score, gm_rank) VALUES (?, ?, ?, ?, ?)",
            )
            .bind(entry.document_id)
            .bind(player_score)
            .bind(player_rank)
            .bind(entry.score)
            .bind(entry.rank)
            .execute(&mut *tx)
            .await
            .context("Failed to persist PageRank score")?;
        }
        tx.commit()
            .await
            .context("Failed to commit PageRank scores")?;
        Ok(PageRankStats {
            document_count: u64::try_from(all_nodes.len()).context("Document count exceeds u64")?,
            player_iterations: player.iterations,
            gm_iterations: gm.iterations,
        })
    }
}
async fn graph_edges(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    player_scope: bool,
) -> Result<Vec<(i64, i64)>> {
    let query = if player_scope {
        "SELECT DISTINCT edge.source_document_id, edge.target_document_id
         FROM document_graph_edges edge
         JOIN note_metadata source ON source.document_id = edge.source_document_id
         JOIN note_metadata target ON target.document_id = edge.target_document_id
         WHERE edge.visibility = 'player'
           AND source.visibility != 'secret'
           AND target.visibility != 'secret'"
    } else {
        "SELECT DISTINCT source_document_id, target_document_id
         FROM document_graph_edges"
    };
    let rows = sqlx::query(query)
        .fetch_all(&mut **tx)
        .await
        .context("Failed to load document graph edges for PageRank")?;
    Ok(rows
        .into_iter()
        .map(|row| (row.get("source_document_id"), row.get("target_document_id")))
        .collect())
}
