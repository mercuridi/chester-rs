mod repository;
mod schema;

#[cfg(test)]
pub(crate) use repository::StructuredNote;
#[cfg(test)]
pub(crate) use repository::register_sqlite_vec;
pub(crate) use repository::{
    AccessScope, IndexedChunk, IndexedDocument, IndexerDb, PageRankSignal, SearchResult,
    StructuredResult,
};
