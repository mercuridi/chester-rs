mod chunker;
mod db;
mod document;
mod embedder;
mod frontmatter;
mod link_resolver;
mod pagerank;
mod prompt;
mod retriever;
mod scanner;
mod schema;
mod service;

#[cfg(test)]
pub(crate) use db::register_sqlite_vec;
pub(crate) use db::{AccessScope, IndexerDb, SearchResult, StructuredResult};
#[cfg(test)]
pub(crate) use db::{IndexedChunk, StructuredNote};
pub(crate) use document::{Chunk, ChunkVisibility, Document};
pub(crate) use embedder::{EMBEDDING_DIMENSIONS, Embedder, EmbeddingModel, MODEL_ID};
#[cfg(test)]
pub(crate) use frontmatter::parse;
pub(crate) use frontmatter::{Metadata, MetadataValue};
pub(crate) use link_resolver::LinkResolution;
pub(crate) use pagerank::compute;
pub(crate) use prompt::build_prompt_with_budget;
#[cfg(test)]
pub(crate) use retriever::{CandidatePoolPolicy, FusionPolicy, RetrievalLimits, SelectionPolicy};
pub(crate) use retriever::{
    RetrievalDiagnostics, RetrievalOutcome, Retriever, RetrieverApi, SearchSettings,
    from_retrieval_config, from_synthesis_config, select_with_diagnostics,
};
pub(crate) use scanner::scan_directory_with_stats;
#[cfg(test)]
pub(crate) use scanner::{CorpusErrorKind, CorpusErrors};
pub(crate) use schema::{
    DOCUMENT_TYPE_DEFINITIONS, FieldDefinition, UNIVERSAL_FIELD_DEFINITIONS, ValueType,
    field_definition, vocabulary_contains,
};
pub(crate) use service::Indexer;
