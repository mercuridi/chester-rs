mod facade;
mod graph;
mod indexing;
mod metadata;
mod search;

#[cfg(test)]
mod tests;

pub(crate) use facade::{
    AccessScope, GraphStats, IndexedChunk, IndexedDocument, IndexerDb, PageRankSignal,
    PageRankStats, SearchResult, StructuredNote, StructuredResult,
};
#[cfg(test)]
pub(crate) use facade::{register_sqlite_vec, versioned_database_url};
