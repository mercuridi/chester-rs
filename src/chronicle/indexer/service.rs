use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{Context, Result};
use tokenizers::Encoding;

use crate::chronicle::indexer::document::Document;
use tracing::{debug, info, instrument, warn};

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

    #[instrument(skip(self), fields(root = %self.root.display()))]
    pub async fn index(&self) -> Result<IndexStats> {
        let (documents, corpus_stats) = scanner::scan_directory_with_stats(&self.root)
            .with_context(|| format!("Failed to scan index directory: {}", self.root.display()))?;

        let indexed_documents = self
            .db
            .all_documents()
            .await
            .context("Failed to load existing index")?;
        info!(
            discovered = documents.len(),
            indexed = indexed_documents.len(),
            "Preparing Chronicle index"
        );
        debug!(
            ?corpus_stats,
            "Collected corpus statistics before embedding"
        );

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
        debug!(
            pending_documents = pending.len(),
            "Indexed changed documents"
        );

        for document in indexed_documents {
            if !seen_paths.contains(&document.path) {
                self.db
                    .delete_document(document.id)
                    .await
                    .with_context(|| {
                        format!("Failed to remove deleted document: {}", document.path)
                    })?;

                stats.removed += 1;
                warn!(path = %document.path, "Removed document from Chronicle index");
            }
        }

        info!(?stats, "Chronicle indexing finished");
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
        debug!(
            documents = pending.len(),
            "Building embeddings for pending documents"
        );
        let mut chunks = pending
            .iter()
            .enumerate()
            .flat_map(|(document_index, (document, _, _))| {
                chunker::chunk(document, self.max_chunk_length)
                    .into_iter()
                    .enumerate()
                    .map(move |(chunk_index, chunk)| {
                        let encoding = self
                            .embedder
                            .encode(&chunk.content)
                            .with_context(|| format!("Failed to tokenize chunk {chunk_index}"))?;

                        Ok((document_index, chunk_index, chunk, encoding))
                    })
            })
            .collect::<Result<Vec<(usize, usize, _, Encoding)>>>()?;

        let total_chunks = chunks.len();
        let total_tokens = chunks
            .iter()
            .map(|(_, _, _, encoding)| encoding.len())
            .sum::<usize>();
        #[allow(clippy::cast_precision_loss)]
        let average_chunk_tokens = if total_chunks == 0 {
            0.0
        } else {
            total_tokens as f64 / total_chunks as f64
        };
        info!(
            document_count = pending.len(),
            chunk_count = total_chunks,
            token_count = total_tokens,
            average_chunk_tokens,
            "Prepared corpus chunks for embedding"
        );

        chunks.sort_by_key(|(_, _, _, encoding)| encoding.len());

        let mut embeddings = pending
            .iter()
            .map(|(document, _, _)| {
                vec![None; chunker::chunk(document, self.max_chunk_length).len()]
            })
            .collect::<Vec<Vec<Option<Vec<f32>>>>>();
        let batch_count = chunks.len().div_ceil(EMBEDDING_BATCH_SIZE);
        for batch in chunks.chunks(EMBEDDING_BATCH_SIZE) {
            debug!(batch_size = batch.len(), "Embedding chunk batch");
            let encodings = batch
                .iter()
                .map(|(_, _, _, encoding)| encoding.clone())
                .collect::<Vec<_>>();
            let batch_embeddings = self
                .embedder
                .embed_encodings(&encodings)
                .context("Failed to embed chunk batch")?;

            for ((document_index, chunk_index, _, _), embedding) in
                batch.iter().zip(batch_embeddings)
            {
                let slot = embeddings
                    .get_mut(*document_index)
                    .and_then(|document_embeddings| document_embeddings.get_mut(*chunk_index))
                    .ok_or_else(|| {
                        anyhow::anyhow!("Embedding batch returned an invalid chunk index")
                    })?;
                if slot.is_some() {
                    anyhow::bail!("Embedding batch returned a duplicate chunk index");
                }
                *slot = Some(embedding);
            }
        }

        info!(
            chunk_count = total_chunks,
            batch_count, "Embedded corpus chunks"
        );

        for ((document, path, updated), document_embeddings) in pending.iter().zip(embeddings) {
            let document_embeddings = document_embeddings
                .into_iter()
                .enumerate()
                .map(|(chunk_index, embedding)| {
                    embedding.ok_or_else(|| {
                        anyhow::anyhow!("Missing embedding for chunk {chunk_index} of {path}")
                    })
                })
                .collect::<Result<Vec<_>>>()?;
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
