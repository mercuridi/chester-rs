use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::chronicle::indexer::document::Document;

use super::{
    chunker,
    db::repository::{IndexedChunk, IndexerDb},
    embedder::Embedder,
    scanner,
};

const EMBEDDING_BATCH_SIZE: usize = 16;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct IndexStats {
    pub added: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub removed: usize,
}

pub struct Indexer {
    root: PathBuf,
    db: IndexerDb,
    embedder: Embedder,
    max_chunk_length: usize,
}

impl Indexer {
    pub fn new(root: PathBuf, db: IndexerDb, embedder: Embedder, max_chunk_length: usize) -> Self {
        Self {
            root,
            db,
            embedder,
            max_chunk_length,
        }
    }

    pub async fn index(&self) -> Result<IndexStats> {
        let documents = scanner::scan_directory(&self.root)
            .with_context(|| format!("Failed to scan index directory: {}", self.root.display()))?;

        let indexed_documents = self
            .db
            .all_documents()
            .await
            .context("Failed to load existing index")?;

        let indexed_by_path = indexed_documents
            .iter()
            .map(|document| (document.path.as_str(), document))
            .collect::<std::collections::HashMap<_, _>>();

        let mut stats = IndexStats::default();
        let mut seen_paths = HashSet::new();
        let mut pending = Vec::new();

        for document in documents {
            let path = document.path.to_string_lossy().into_owned();
            seen_paths.insert(path.clone());

            if let Some(indexed) = indexed_by_path.get(path.as_str()) {
                if indexed.content_hash == document.content_hash {
                    stats.unchanged += 1;
                    continue;
                }

                pending.push((document, path, true));
            } else {
                pending.push((document, path, false));
            }
        }

        self.index_pending_documents(&mut pending, &mut stats)
            .await?;

        for document in indexed_documents {
            if !seen_paths.contains(&document.path) {
                self.db
                    .delete_document(document.id)
                    .await
                    .with_context(|| {
                        format!("Failed to remove deleted document: {}", document.path)
                    })?;

                stats.removed += 1;
            }
        }

        Ok(stats)
    }

    pub fn into_parts(self) -> (IndexerDb, Embedder) {
        (self.db, self.embedder)
    }

    async fn index_pending_documents(
        &self,
        pending: &mut [(Document, String, bool)],
        stats: &mut IndexStats,
    ) -> Result<()> {
        let chunks = pending
            .iter()
            .enumerate()
            .flat_map(|(document_index, (document, _, _))| {
                chunker::chunk(document, self.max_chunk_length)
                    .into_iter()
                    .enumerate()
                    .map(move |(chunk_index, chunk)| (document_index, chunk_index, chunk))
            })
            .collect::<Vec<_>>();

        let mut embeddings = vec![Vec::new(); pending.len()];
        for batch in chunks.chunks(EMBEDDING_BATCH_SIZE) {
            let texts = batch
                .iter()
                .map(|(_, _, chunk)| chunk.content.as_str())
                .collect::<Vec<_>>();
            let batch_embeddings = self
                .embedder
                .embed_batch(&texts)
                .context("Failed to embed chunk batch")?;

            for ((document_index, chunk_index, _), embedding) in batch.iter().zip(batch_embeddings)
            {
                if embeddings[*document_index].len() != *chunk_index {
                    anyhow::bail!("Embedding batch returned chunks out of order");
                }
                embeddings[*document_index].push(embedding);
            }
        }

        for ((document, path, updated), document_embeddings) in pending.iter().zip(embeddings) {
            self.index_document(document, path, &document_embeddings)
                .await?;
            if *updated {
                stats.updated += 1;
            } else {
                stats.added += 1;
            }
        }

        Ok(())
    }

    async fn index_document(
        &self,
        document: &Document,
        path: &str,
        embeddings: &[Vec<f32>],
    ) -> Result<()> {
        let chunks = chunker::chunk(document, self.max_chunk_length);

        let indexed_chunks = chunks
            .into_iter()
            .map(|chunk| {
                Ok(IndexedChunk {
                    chunk_index: i64::try_from(chunk.index)
                        .context("Chunk index does not fit in SQLite integer")?,
                    heading: chunk.heading,
                    text: chunk.content,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        self.db
            .replace_document(path, &document.content_hash, &indexed_chunks, embeddings)
            .await
            .with_context(|| format!("Failed to persist document: {path}"))?;

        Ok(())
    }
}
