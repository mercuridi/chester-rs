use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use tokenizers::Encoding;

use crate::chronicle::{
    indexer::document::{Chunk, ChunkVisibility, Document},
    runtime::report_cuda_oom,
};
use tracing::{debug, info, instrument, warn};

use super::{
    chunker,
    db::repository::facade::{IndexedChunk, IndexedDocument, IndexerDb},
    embedder::{Embedder, EmbeddingModel},
    link_resolver, scanner,
};

#[cfg(test)]
use super::db::repository::facade::AccessScope;

const EMBEDDING_BATCH_SIZE: usize = 16;
const PREPARATION_BATCH_DOCUMENTS: usize = 8;

struct PreparedChunk {
    chunk: Chunk,
    encoding: Encoding,
    embedding: Option<Vec<f32>>,
}

struct PreparedDocument {
    chunks: Vec<PreparedChunk>,
}

impl PreparedDocument {
    fn chunks(
        document: &Document,
        tokenizer: &tokenizers::Tokenizer,
        max_chunk_tokens: usize,
        chunk_overlap_tokens: usize,
    ) -> Result<Vec<Chunk>> {
        let mut chunks =
            chunker::chunk::chunk(document, tokenizer, max_chunk_tokens, chunk_overlap_tokens)?;
        let primary_visibility = if document.metadata.visibility == "secret" {
            ChunkVisibility::Secret
        } else {
            ChunkVisibility::Player
        };
        for chunk in &mut chunks {
            chunk.visibility = primary_visibility;
        }
        for secret_content in &document.secret_content {
            let mut secret_document = document.clone();
            secret_document.content.clone_from(secret_content);
            secret_document.secret_content.clear();
            let offset = chunks.len();
            let mut secret_chunks = chunker::chunk::chunk(
                &secret_document,
                tokenizer,
                max_chunk_tokens,
                chunk_overlap_tokens,
            )?;
            for (index, chunk) in secret_chunks.iter_mut().enumerate() {
                chunk.index = offset + index;
                chunk.visibility = ChunkVisibility::Secret;
            }
            chunks.extend(secret_chunks);
        }
        Ok(chunks)
    }

    fn prepare(
        document: &Document,
        embedder: &dyn EmbeddingModel,
        max_chunk_tokens: usize,
        chunk_overlap_tokens: usize,
    ) -> Result<Self> {
        let chunks = Self::chunks(
            document,
            embedder.chunking_tokenizer(),
            max_chunk_tokens,
            chunk_overlap_tokens,
        )?;
        let chunks = chunks
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
                visibility: prepared.chunk.visibility,
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
    pub graph_rebuilt: bool,
    pub pagerank_rebuilt: bool,
}

struct ResolvedCorpus {
    candidates: Vec<scanner::DocumentCandidate>,
    corpus_stats: scanner::CorpusStats,
    link_resolution: link_resolver::LinkResolution,
    graph_fingerprint: String,
}

pub struct Indexer {
    root: PathBuf,
    db: IndexerDb,
    embedder: Box<dyn EmbeddingModel>,
    max_chunk_tokens: usize,
    chunk_overlap_tokens: usize,
    excluded_note_ids: HashSet<String>,
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
            embedder: Box::new(embedder),
            max_chunk_tokens,
            chunk_overlap_tokens,
            excluded_note_ids: HashSet::new(),
        }
    }

    pub fn with_excluded_note_ids(mut self, excluded_note_ids: HashSet<String>) -> Self {
        self.excluded_note_ids = excluded_note_ids;
        self
    }

    #[cfg(test)]
    pub fn with_embedding_model(
        root: PathBuf,
        db: IndexerDb,
        embedder: Box<dyn EmbeddingModel>,
        max_chunk_tokens: usize,
        chunk_overlap_tokens: usize,
    ) -> Self {
        Self {
            root,
            db,
            embedder,
            max_chunk_tokens,
            chunk_overlap_tokens,
            excluded_note_ids: HashSet::new(),
        }
    }

    #[instrument(skip(self), fields(root = %self.root.display()))]
    pub async fn index(&self) -> Result<IndexStats> {
        let corpus = self.scan_and_resolve_corpus()?;
        let indexed_documents = self
            .db
            .all_documents()
            .await
            .context("Failed to load existing index")?;
        info!(
            discovered = corpus.candidates.len(),
            indexed = indexed_documents.len(),
            "Preparing Chronicle index"
        );
        debug!(
            ?corpus.corpus_stats,
            "Collected corpus statistics before embedding"
        );
        let (mut stats, seen_paths) = self
            .index_discovered_documents(corpus.candidates, &indexed_documents)
            .await?;
        debug!(
            pending_documents = stats.added + stats.updated,
            "Indexed changed documents"
        );

        self.remove_deleted_documents(&indexed_documents, &seen_paths, &mut stats)
            .await?;
        self.rebuild_graph_if_changed(
            &corpus.graph_fingerprint,
            &corpus.link_resolution,
            &mut stats,
        )
        .await?;

        info!(?stats, "Chronicle indexing finished");
        Ok(stats)
    }

    fn scan_and_resolve_corpus(&self) -> Result<ResolvedCorpus> {
        let (candidates, corpus_stats) = scanner::discover_directory_with_stats_excluding(
            &self.root,
            &self.excluded_note_ids,
        )
        .with_context(|| format!("Failed to scan index directory: {}", self.root.display()))?;
        let resolver_catalogue = link_resolver::catalogue_from_candidates(&self.root, &candidates)?;
        let mut link_resolution = link_resolver::LinkResolution::default();
        for candidate in &candidates {
            let document = scanner::load_document(candidate)?;
            let resolved = link_resolver::resolve_document(&resolver_catalogue, &document);
            link_resolution.resolved.extend(resolved.resolved);
            link_resolution.dangling.extend(resolved.dangling);
            link_resolution.ambiguous.extend(resolved.ambiguous);
        }
        debug!(
            resolved = link_resolution.resolved.len(),
            dangling = link_resolution.dangling.len(),
            ambiguous = link_resolution.ambiguous.len(),
            "Resolved Chronicle wikilinks"
        );
        let graph_fingerprint = graph_input_fingerprint(&candidates, &link_resolution);
        Ok(ResolvedCorpus {
            candidates,
            corpus_stats,
            link_resolution,
            graph_fingerprint,
        })
    }

    async fn index_discovered_documents(
        &self,
        candidates: Vec<scanner::DocumentCandidate>,
        indexed_documents: &[IndexedDocument],
    ) -> Result<(IndexStats, HashSet<String>)> {
        let indexed_by_path = indexed_documents
            .iter()
            .map(|document| (document.path.as_str(), document))
            .collect::<std::collections::HashMap<_, _>>();
        let mut stats = IndexStats::default();
        let mut seen_paths = HashSet::new();
        let mut pending = Vec::new();

        for candidate in candidates {
            let path = candidate.path.to_string_lossy().into_owned();
            seen_paths.insert(path.clone());

            if let Some(indexed) = indexed_by_path.get(path.as_str()) {
                let fingerprint = index_fingerprint_candidate(
                    &candidate,
                    self.max_chunk_tokens,
                    self.chunk_overlap_tokens,
                );
                let unchanged = if indexed.content_hash == fingerprint {
                    true
                } else {
                    let document = scanner::load_document(&candidate)?;
                    let chunks = PreparedDocument::chunks(
                        &document,
                        self.embedder.chunking_tokenizer(),
                        self.max_chunk_tokens,
                        self.chunk_overlap_tokens,
                    )?;
                    self.db.chunks_match(indexed.id, &chunks).await?
                };
                if unchanged {
                    if !self
                        .db
                        .metadata_matches(indexed.id, &candidate.metadata)
                        .await?
                    {
                        self.db
                            .refresh_metadata(indexed.id, &fingerprint, &candidate.metadata)
                            .await?;
                    }
                    stats.unchanged += 1;
                    continue;
                }

                let document = scanner::load_document(&candidate)?;
                pending.push((document, path, true));
            } else {
                let document = scanner::load_document(&candidate)?;
                pending.push((document, path, false));
            }

            if pending.len() == PREPARATION_BATCH_DOCUMENTS {
                self.index_pending_documents(&pending, &mut stats).await?;
                pending.clear();
            }
        }

        if !pending.is_empty() {
            self.index_pending_documents(&pending, &mut stats).await?;
        }
        Ok((stats, seen_paths))
    }

    async fn remove_deleted_documents(
        &self,
        indexed_documents: &[IndexedDocument],
        seen_paths: &HashSet<String>,
        stats: &mut IndexStats,
    ) -> Result<()> {
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
        Ok(())
    }

    async fn rebuild_graph_if_changed(
        &self,
        graph_fingerprint: &str,
        link_resolution: &link_resolver::LinkResolution,
        stats: &mut IndexStats,
    ) -> Result<()> {
        let graph_changed =
            self.db.graph_input_fingerprint().await?.as_deref() != Some(graph_fingerprint);
        if graph_changed {
            stats.graph_rebuilt = true;
            let graph_stats = self.db.rebuild_document_graph(link_resolution).await?;
            debug!(
                edges = graph_stats.edge_count,
                "Persisted resolved Chronicle document graph"
            );
            let pagerank_stats = self.db.rebuild_document_pagerank().await?;
            stats.pagerank_rebuilt = true;
            debug!(
                documents = pagerank_stats.document_count,
                player_iterations = pagerank_stats.player_iterations,
                gm_iterations = pagerank_stats.gm_iterations,
                "Rebuilt Chronicle document PageRank"
            );
            self.db
                .set_graph_input_fingerprint(graph_fingerprint)
                .await?;
        } else {
            debug!("Skipped unchanged Chronicle document graph and PageRank");
        }
        Ok(())
    }

    pub fn into_parts(self) -> (IndexerDb, Box<dyn EmbeddingModel>) {
        (self.db, self.embedder)
    }

    async fn index_pending_documents(
        &self,
        pending: &[(Document, String, bool)],
        stats: &mut IndexStats,
    ) -> Result<()> {
        for batch in pending.chunks(PREPARATION_BATCH_DOCUMENTS) {
            self.index_pending_documents_batch(batch, stats).await?;
        }
        Ok(())
    }

    async fn index_pending_documents_batch(
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
                    self.embedder.as_ref(),
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
                .inspect_err(|error| {
                    report_cuda_oom(error, "embedding", "batch");
                })
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
            .replace_note(
                path,
                &index_fingerprint(document, self.max_chunk_tokens, self.chunk_overlap_tokens),
                &indexed_chunks,
                &embeddings,
                &document.metadata,
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
        "{}:chunker-v10-clean-frontmatter:{max_chunk_tokens}:overlap:{chunk_overlap_tokens}",
        document.content_hash
    )
}

fn graph_input_fingerprint(
    candidates: &[scanner::DocumentCandidate],
    resolution: &link_resolver::LinkResolution,
) -> String {
    let mut hasher = Sha256::new();
    // Scanner order is path-stable; include only graph-relevant authored data
    // plus the complete resolver result, including dangling/ambiguous links.
    for document in candidates {
        hasher.update(document.path.to_string_lossy().as_bytes());
        hasher.update([0]);
        hasher.update(format!(
            "{}\0{}\0{}",
            document.metadata.id,
            document.metadata.visibility,
            document.metadata.aliases.join("\0"),
        ));
    }
    hasher.update(format!("{resolution:?}"));
    hex::encode(hasher.finalize())
}

fn index_fingerprint_candidate(
    candidate: &scanner::DocumentCandidate,
    max_chunk_tokens: usize,
    chunk_overlap_tokens: usize,
) -> String {
    format!(
        "{}:chunker-v10-clean-frontmatter:{max_chunk_tokens}:overlap:{chunk_overlap_tokens}",
        candidate.content_hash
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokenizers::Token;

    struct CountingEmbedder {
        tokenizer: tokenizers::Tokenizer,
        batches: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }
    impl EmbeddingModel for CountingEmbedder {
        fn chunking_tokenizer(&self) -> &tokenizers::Tokenizer {
            &self.tokenizer
        }
        fn encode(&self, text: &str) -> Result<Encoding> {
            self.tokenizer
                .encode(text, true)
                .map_err(|e| anyhow::anyhow!("{e}"))
        }
        fn embed_encodings(&self, encodings: &[Encoding]) -> Result<Vec<Vec<f32>>> {
            self.batches
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(vec![
                vec![0.0; super::super::embedder::EMBEDDING_DIMENSIONS];
                encodings.len()
            ])
        }
    }

    #[tokio::test]
    async fn metadata_backfill_and_changes_reuse_embeddings_and_remove_noncanon_notes() -> Result<()>
    {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        use tokenizers::{models::wordlevel::WordLevel, pre_tokenizers::whitespace::Whitespace};
        let temp = tempfile::tempdir()?;
        let corpus = temp.path().join("corpus");
        std::fs::create_dir(&corpus)?;
        let path = corpus.join("Ada.md");
        let source = "---\nid: ada\ntype: character\nstatus: canon\nvisibility: player\ncreated: 2026-09-07\nupdated: 2026-09-07\nrole: npc\nlife_status: alive\n---\nAda tends a garden.\n";
        std::fs::write(&path, source)?;
        let model = WordLevel::builder()
            .vocab([("[UNK]".into(), 0)].into_iter().collect())
            .unk_token("[UNK]".into())
            .build()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut tokenizer = tokenizers::Tokenizer::new(model);
        tokenizer.with_pre_tokenizer(Some(Whitespace {}));
        let batches = Arc::new(AtomicUsize::new(0));
        let db = IndexerDb::open(&format!(
            "sqlite://{}",
            temp.path().join("test.sqlite3").display()
        ))
        .await?;
        let indexer = Indexer::with_embedding_model(
            corpus,
            db.clone(),
            Box::new(CountingEmbedder {
                tokenizer,
                batches: batches.clone(),
            }),
            128,
            0,
        );
        indexer.index().await?;
        let initial_batches = batches.load(Ordering::SeqCst);
        assert!(initial_batches > 0);
        let docs = db.all_documents().await?;
        let (mut metadata, _) = super::super::frontmatter::parse(source)?.context("note")?;
        // Simulate an existing index whose generic field row has not been populated.
        metadata.fields.remove("role");
        db.refresh_metadata(docs[0].id, &docs[0].content_hash, &metadata)
            .await?;
        let second = indexer.index().await?;
        assert_eq!(second.unchanged, 1);
        assert!(!second.graph_rebuilt);
        assert!(!second.pagerank_rebuilt);
        let plan = crate::chronicle::query::plan::StructuredPlan::try_from(
            crate::chronicle::query::planner::parse(
                r#"{"operation":"count","note_type":"character","filters":{"conditions":[{"field":"role","operator":"equals","value":"npc"}]}}"#,
            )?,
        )?;
        assert_eq!(db.execute_plan_for(&plan, AccessScope::Gm).await?.total, 1);
        std::fs::write(&path, source.replace("role: npc", "role: pc"))?;
        let metadata_change = indexer.index().await?;
        assert_eq!(metadata_change.unchanged, 1);
        assert!(!metadata_change.graph_rebuilt);
        assert!(!metadata_change.pagerank_rebuilt);
        assert_eq!(db.execute_plan_for(&plan, AccessScope::Gm).await?.total, 0);
        assert_eq!(batches.load(Ordering::SeqCst), initial_batches);
        std::fs::write(&path, source.replace("status: canon", "status: draft"))?;
        assert_eq!(indexer.index().await?.removed, 1);
        assert!(!db.has_chunks().await?);
        assert!(
            db.search_lexical_for("garden", 10, AccessScope::Gm)
                .await?
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn visibility_transitions_reindex_all_chunks_and_vectors() -> Result<()> {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        use tokenizers::{models::wordlevel::WordLevel, pre_tokenizers::whitespace::Whitespace};

        let temp = tempfile::tempdir()?;
        let corpus = temp.path().join("corpus");
        std::fs::create_dir(&corpus)?;
        let path = corpus.join("Visibility.md");
        let model = WordLevel::builder()
            .vocab([("[UNK]".into(), 0)].into_iter().collect())
            .unk_token("[UNK]".into())
            .build()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut tokenizer = tokenizers::Tokenizer::new(model);
        tokenizer.with_pre_tokenizer(Some(Whitespace {}));
        let batches = Arc::new(AtomicUsize::new(0));
        let db = IndexerDb::open(&format!(
            "sqlite://{}",
            temp.path().join("test.sqlite3").display()
        ))
        .await?;
        let indexer = Indexer::with_embedding_model(
            corpus,
            db.clone(),
            Box::new(CountingEmbedder {
                tokenizer,
                batches: batches.clone(),
            }),
            128,
            0,
        );

        let source = |visibility: &str, callout: bool| {
            let secret = if callout {
                "\n> [!secret] Hidden\n> secrettoken\n"
            } else {
                ""
            };
            format!(
                "---\nid: visibility\ntype: lore\nstatus: canon\nvisibility: {visibility}\ncreated: 2026-09-07\nupdated: 2026-09-07\n---\npublictoken{secret}"
            )
        };

        let transitions = [
            ("player", false, "secret", false),
            ("secret", false, "player", false),
            ("player", false, "mixed", true),
            ("mixed", true, "player", false),
            ("player", false, "mixed", true),
            ("mixed", true, "secret", false),
        ];
        std::fs::write(&path, source("player", false))?;
        indexer.index().await?;

        for (from_visibility, _from_callout, to_visibility, to_callout) in transitions {
            std::fs::write(&path, source(to_visibility, to_callout))?;
            let stats = indexer.index().await?;
            assert_eq!(stats.updated, 1, "{from_visibility} -> {to_visibility}");
            assert_eq!(stats.unchanged, 0, "{from_visibility} -> {to_visibility}");

            let public_player_matches = db
                .search_lexical_for("publictoken", 10, AccessScope::Player)
                .await?;
            assert_eq!(
                public_player_matches.len(),
                usize::from(to_visibility != "secret")
            );
            assert_eq!(
                db.search_lexical_for("publictoken", 10, AccessScope::Gm)
                    .await?
                    .len(),
                1
            );
            assert!(
                db.search_lexical_for("secrettoken", 10, AccessScope::Player)
                    .await?
                    .is_empty(),
                "player search leaked secrettoken during {from_visibility} -> {to_visibility}"
            );
            let secret_gm_matches = db
                .search_lexical_for("secrettoken", 10, AccessScope::Gm)
                .await?;
            assert_eq!(
                secret_gm_matches.len(),
                usize::from(to_visibility == "mixed"),
                "GM search did not find secrettoken during {from_visibility} -> {to_visibility}"
            );
        }

        assert_eq!(batches.load(Ordering::SeqCst), 7);
        Ok(())
    }

    fn prepared_chunk(index: usize, text: &str, embedding: Option<Vec<f32>>) -> PreparedChunk {
        PreparedChunk {
            chunk: Chunk {
                document_path: "prepared.md".into(),
                index,
                content: text.to_owned(),
                visibility: ChunkVisibility::Player,
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

    #[test]
    fn empty_prepared_document_converts_to_empty_index_data() -> Result<()> {
        let (chunks, embeddings) =
            PreparedDocument { chunks: Vec::new() }.into_index_data("empty.md")?;
        assert!(chunks.is_empty());
        assert!(embeddings.is_empty());
        Ok(())
    }

    #[test]
    fn index_fingerprint_includes_content_and_chunking_configuration() {
        let document = Document {
            metadata: crate::chronicle::indexer::frontmatter::Metadata::default(),
            path: "doc.md".into(),
            content: "content".into(),
            public_body: String::new(),
            secret_content: Vec::new(),
            secret_bodies: Vec::new(),
            content_hash: "hash".into(),
        };
        let baseline = index_fingerprint(&document, 100, 10);
        assert!(baseline.starts_with("hash:chunker-v10-clean-frontmatter:"));
        assert_ne!(baseline, index_fingerprint(&document, 101, 10));
        assert_ne!(baseline, index_fingerprint(&document, 100, 11));

        let changed = Document {
            content_hash: "other".into(),
            ..document
        };
        assert_ne!(baseline, index_fingerprint(&changed, 100, 10));
    }
}
