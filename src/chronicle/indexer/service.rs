use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{Context, Result};
use tokenizers::Encoding;

use crate::chronicle::indexer::document::{Chunk, Document};
use tracing::{debug, info, instrument, warn};

use super::{
    chunker,
    db::repository::{IndexedChunk, IndexerDb},
    embedder::Embedder,
    scanner,
};

const EMBEDDING_BATCH_SIZE: usize = 16;

struct PreparedChunk {
    chunk: Chunk,
    encoding: Encoding,
    embedding: Option<Vec<f32>>,
}

struct PreparedDocument {
    chunks: Vec<PreparedChunk>,
}

impl PreparedDocument {
    fn prepare(
        document: &Document,
        embedder: &Embedder,
        max_chunk_tokens: usize,
        chunk_overlap_tokens: usize,
    ) -> Result<Self> {
        let chunks = chunker::chunk(
            document,
            embedder.chunking_tokenizer(),
            max_chunk_tokens,
            chunk_overlap_tokens,
        )?
        .into_iter()
        .map(|chunk| {
            let encoding = embedder
                .encode(&chunk.content)
                .with_context(|| format!("Failed to tokenize chunk {}", chunk.index))?;
            Ok(PreparedChunk {
                chunk,
                encoding,
                embedding: None,
            })
        })
        .collect::<Result<Vec<_>>>()?;

        Ok(Self { chunks })
    }

    fn into_index_data(self, path: &str) -> Result<(Vec<IndexedChunk>, Vec<Vec<f32>>)> {
        let mut indexed_chunks = Vec::with_capacity(self.chunks.len());
        let mut embeddings = Vec::with_capacity(self.chunks.len());

        for prepared in self.chunks {
            let chunk_index = prepared.chunk.index;
            indexed_chunks.push(IndexedChunk {
                chunk_index: i64::try_from(chunk_index)
                    .context("Chunk index does not fit in SQLite integer")?,
                heading: prepared.chunk.heading,
                text: prepared.chunk.content,
                overlaps_previous: prepared.chunk.overlap_tokens > 0,
            });
            embeddings.push(prepared.embedding.ok_or_else(|| {
                anyhow::anyhow!("Missing embedding for chunk {chunk_index} of {path}")
            })?);
        }

        Ok((indexed_chunks, embeddings))
    }
}

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
    max_chunk_tokens: usize,
    chunk_overlap_tokens: usize,
}

impl Indexer {
    pub fn new(
        root: PathBuf,
        db: IndexerDb,
        embedder: Embedder,
        max_chunk_tokens: usize,
        chunk_overlap_tokens: usize,
    ) -> Self {
        Self {
            root,
            db,
            embedder,
            max_chunk_tokens,
            chunk_overlap_tokens,
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
                if indexed.content_hash
                    == index_fingerprint(
                        &document,
                        self.max_chunk_tokens,
                        self.chunk_overlap_tokens,
                    )
                {
                    stats.unchanged += 1;
                    continue;
                }

                pending.push((document, path, true));
            } else {
                pending.push((document, path, false));
            }
        }

        self.index_pending_documents(&pending, &mut stats).await?;
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
        pending: &[(Document, String, bool)],
        stats: &mut IndexStats,
    ) -> Result<()> {
        debug!(
            documents = pending.len(),
            "Building embeddings for pending documents"
        );
        let mut prepared = pending
            .iter()
            .map(|(document, path, _)| {
                PreparedDocument::prepare(
                    document,
                    &self.embedder,
                    self.max_chunk_tokens,
                    self.chunk_overlap_tokens,
                )
                .with_context(|| format!("Failed to prepare chunks for {path}"))
            })
            .collect::<Result<Vec<_>>>()?;

        let total_chunks = prepared
            .iter()
            .map(|document| document.chunks.len())
            .sum::<usize>();
        log_chunk_metrics(&prepared, pending.len(), self.chunk_overlap_tokens);

        let mut embedding_order = prepared
            .iter()
            .enumerate()
            .flat_map(|(document_index, document)| {
                (0..document.chunks.len()).map(move |chunk_index| (document_index, chunk_index))
            })
            .collect::<Vec<_>>();
        embedding_order.sort_by_key(|(document_index, chunk_index)| {
            prepared[*document_index].chunks[*chunk_index]
                .encoding
                .len()
        });

        let batch_count = embedding_order.len().div_ceil(EMBEDDING_BATCH_SIZE);
        for batch in embedding_order.chunks(EMBEDDING_BATCH_SIZE) {
            debug!(batch_size = batch.len(), "Embedding chunk batch");
            let encodings = batch
                .iter()
                .map(|(document_index, chunk_index)| {
                    prepared[*document_index].chunks[*chunk_index]
                        .encoding
                        .clone()
                })
                .collect::<Vec<_>>();
            let batch_embeddings = self
                .embedder
                .embed_encodings(&encodings)
                .context("Failed to embed chunk batch")?;
            if batch_embeddings.len() != batch.len() {
                anyhow::bail!(
                    "Embedding batch returned {} embeddings for {} chunks",
                    batch_embeddings.len(),
                    batch.len()
                );
            }

            for ((document_index, chunk_index), embedding) in batch.iter().zip(batch_embeddings) {
                let slot = prepared
                    .get_mut(*document_index)
                    .and_then(|document| document.chunks.get_mut(*chunk_index))
                    .ok_or_else(|| {
                        anyhow::anyhow!("Embedding batch returned an invalid chunk index")
                    })?;
                if slot.embedding.is_some() {
                    anyhow::bail!("Embedding batch returned a duplicate chunk index");
                }
                slot.embedding = Some(embedding);
            }
        }

        info!(
            chunk_count = total_chunks,
            batch_count, "Embedded corpus chunks"
        );

        for ((document, path, updated), prepared_document) in pending.iter().zip(prepared) {
            self.persist_document(document, path, prepared_document)
                .await?;
            if *updated {
                stats.updated += 1;
            } else {
                stats.added += 1;
            }
        }

        Ok(())
    }

    async fn persist_document(
        &self,
        document: &Document,
        path: &str,
        prepared: PreparedDocument,
    ) -> Result<()> {
        let (indexed_chunks, embeddings) = prepared.into_index_data(path)?;

        self.db
            .replace_document(
                path,
                &index_fingerprint(document, self.max_chunk_tokens, self.chunk_overlap_tokens),
                &indexed_chunks,
                &embeddings,
            )
            .await
            .with_context(|| format!("Failed to persist document: {path}"))?;

        Ok(())
    }
}

fn log_chunk_metrics(
    documents: &[PreparedDocument],
    document_count: usize,
    requested_overlap_tokens: usize,
) {
    let chunks = || documents.iter().flat_map(|document| &document.chunks);
    let chunk_count = chunks().count();
    let token_count = chunks().map(|chunk| chunk.encoding.len()).sum::<usize>();
    let eligible_overlap_boundaries = chunks()
        .filter(|chunk| chunk.chunk.overlap_eligible)
        .count();
    let overlapped_boundaries = chunks()
        .filter(|chunk| chunk.chunk.overlap_tokens > 0)
        .count();
    let overlap_shortfall_boundaries = chunks()
        .filter(|chunk| {
            chunk.chunk.overlap_eligible && chunk.chunk.overlap_tokens < requested_overlap_tokens
        })
        .count();
    let total_overlap_tokens = chunks()
        .map(|chunk| chunk.chunk.overlap_tokens)
        .sum::<usize>();
    #[allow(clippy::cast_precision_loss)]
    let average_chunk_tokens = if chunk_count == 0 {
        0.0
    } else {
        token_count as f64 / chunk_count as f64
    };
    #[allow(clippy::cast_precision_loss)]
    let average_overlap_tokens = if eligible_overlap_boundaries == 0 {
        0.0
    } else {
        total_overlap_tokens as f64 / eligible_overlap_boundaries as f64
    };

    info!(
        document_count,
        chunk_count,
        token_count,
        average_chunk_tokens,
        requested_overlap_tokens,
        eligible_overlap_boundaries,
        overlapped_boundaries,
        overlap_shortfall_boundaries,
        total_overlap_tokens,
        average_overlap_tokens,
        "Prepared corpus chunks for embedding"
    );
}

fn index_fingerprint(
    document: &Document,
    max_chunk_tokens: usize,
    chunk_overlap_tokens: usize,
) -> String {
    format!(
        "{}:chunker-v8-overlap-provenance:{max_chunk_tokens}:overlap:{chunk_overlap_tokens}",
        document.content_hash
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokenizers::Token;

    fn prepared_chunk(index: usize, text: &str, embedding: Option<Vec<f32>>) -> PreparedChunk {
        PreparedChunk {
            chunk: Chunk {
                document_path: "prepared.md".into(),
                index,
                content: text.to_owned(),
                heading: Some("Prepared".to_owned()),
                overlap_eligible: index > 0,
                overlap_tokens: usize::from(index > 0),
            },
            encoding: Encoding::from_tokens(
                vec![Token::new(1, text.to_owned(), (0, text.len()))],
                0,
            ),
            embedding,
        }
    }

    #[test]
    fn prepared_chunks_keep_stored_text_and_embeddings_aligned() -> Result<()> {
        let prepared = PreparedDocument {
            chunks: vec![
                prepared_chunk(0, "first", Some(vec![1.0, 2.0])),
                prepared_chunk(1, "second", Some(vec![3.0, 4.0])),
            ],
        };

        let (chunks, embeddings) = prepared.into_index_data("prepared.md")?;

        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].chunk_index, 0);
        assert_eq!(chunks[0].text, "first");
        assert!(!chunks[0].overlaps_previous);
        assert_eq!(chunks[1].chunk_index, 1);
        assert_eq!(chunks[1].text, "second");
        assert!(chunks[1].overlaps_previous);
        assert_eq!(embeddings, vec![vec![1.0, 2.0], vec![3.0, 4.0]]);
        Ok(())
    }

    #[test]
    fn prepared_document_rejects_a_missing_embedding() -> Result<()> {
        let prepared = PreparedDocument {
            chunks: vec![prepared_chunk(0, "missing", None)],
        };

        let error = prepared
            .into_index_data("prepared.md")
            .err()
            .ok_or_else(|| anyhow::anyhow!("Missing embedding should be rejected"))?;

        assert!(error.to_string().contains("chunk 0 of prepared.md"));
        Ok(())
    }
}
