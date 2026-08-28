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

        for document in documents {
            let path = document.path.to_string_lossy().into_owned();
            seen_paths.insert(path.clone());

            if let Some(indexed) = indexed_by_path.get(path.as_str()) {
                if indexed.content_hash == document.content_hash {
                    stats.unchanged += 1;
                    continue;
                }

                self.index_document(&document, &path).await?;

                stats.updated += 1;
            } else {
                self.index_document(&document, &path).await?;

                stats.added += 1;
            }
        }

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

    async fn index_document(&self, document: &Document, path: &str) -> Result<()> {
        let chunks = chunker::chunk(document, self.max_chunk_length)
            .with_context(|| format!("Failed to chunk document: {path}"))?;

        let embeddings = chunks
            .iter()
            .map(|chunk| {
                self.embedder
                    .embed(&chunk.content)
                    .with_context(|| format!("Failed to embed chunk {} of {path}", chunk.index))
            })
            .collect::<Result<Vec<_>>>()?;

        let indexed_chunks = chunks
            .into_iter()
            .map(|chunk| IndexedChunk {
                chunk_index: chunk.index as i64,
                heading: chunk.heading,
                text: chunk.content,
            })
            .collect::<Vec<_>>();

        self.db
            .replace_document(path, &document.content_hash, &indexed_chunks, &embeddings)
            .await
            .with_context(|| format!("Failed to persist document: {path}"))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn failed_document_replacement_preserves_existing_document() -> Result<()> {
        let db = IndexerDb::open(":memory:").await?;

        let chunks = vec![IndexedChunk {
            chunk_index: 0,
            heading: None,
            text: "Original text".to_owned(),
        }];

        let embeddings = vec![vec![0.1_f32; 384]];

        db.replace_document("test.md", "original-hash", &chunks, &embeddings)
            .await?;

        // Deliberately provide mismatched data. The operation must fail
        // before modifying the existing document.
        let result = db
            .replace_document("test.md", "new-hash", &chunks, &[])
            .await;

        assert!(result.is_err());

        let document = db
            .document_by_path("test.md")
            .await?
            .expect("document should still exist");

        assert_eq!(document.content_hash, "original-hash");

        Ok(())
    }
}
