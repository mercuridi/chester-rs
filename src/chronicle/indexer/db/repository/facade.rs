//! Indexer database facade.
//!
//! Capability implementations live in sibling modules so graph, retrieval, and
//! structured-metadata work can evolve independently while callers keep using
//! `IndexerDb`.

use std::path::Path;

use anyhow::Result;
use sqlx::sqlite::SqlitePool;

use crate::chronicle::indexer::db::schema::{INDEX_FORMAT_VERSION, initialise};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AccessScope {
    Player,
    Gm,
}

impl AccessScope {
    pub const fn is_gm(self) -> bool {
        matches!(self, Self::Gm)
    }
}

#[derive(Debug, Clone)]
pub struct IndexedDocument {
    pub id: i64,
    pub path: String,
    pub content_hash: String,
}
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct GraphStats {
    pub edge_count: u64,
}
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PageRankStats {
    pub document_count: u64,
    pub player_iterations: usize,
    pub gm_iterations: usize,
}
#[derive(Debug, Clone, Copy, Default)]
pub struct PageRankSignal {
    pub score: f64,
    pub rank: i64,
}
#[derive(Debug, Clone)]
pub struct IndexedChunk {
    pub chunk_index: i64,
    pub heading: Option<String>,
    pub text: String,
    pub visibility: crate::chronicle::indexer::document::ChunkVisibility,
    /// Whether this chunk contains content repeated from its immediate predecessor.
    pub overlaps_previous: bool,
}
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub document_path: String,
    pub chunk_index: i64,
    pub heading: Option<String>,
    pub text: String,
    pub overlaps_previous: bool,
    pub distance: f32,
}
#[derive(Debug, serde::Serialize)]
pub struct StructuredNote {
    pub id: String,
    pub title: String,
}
#[derive(Debug, serde::Serialize)]
pub struct StructuredResult {
    pub total: i64,
    pub notes: Vec<StructuredNote>,
}

#[derive(Clone)]
pub struct IndexerDb {
    pub(super) pool: SqlitePool,
}

impl IndexerDb {
    pub async fn open(path: &str) -> Result<Self> {
        register_sqlite_vec();
        let database_url = versioned_database_url(path);
        let pool = crate::database::pool::open_sqlite_pool(&database_url, "Chronicle").await?;
        initialise(&pool).await?;
        Ok(Self { pool })
    }
}

/// Returns the cache location for the current derived-index format.
pub fn versioned_database_url(database_url: &str) -> String {
    let Some(path_and_query) = database_url.strip_prefix("sqlite://") else {
        return database_url.to_owned();
    };
    let (path, query) = path_and_query
        .split_once('?')
        .map_or((path_and_query, None), |(path, query)| (path, Some(query)));
    if path == ":memory:" {
        return database_url.to_owned();
    }
    let path = Path::new(path);
    let (Some(stem), Some(extension)) = (path.file_stem(), path.extension()) else {
        return database_url.to_owned();
    };
    let filename = format!(
        "{}.index-v{}.{}",
        stem.to_string_lossy(),
        INDEX_FORMAT_VERSION,
        extension.to_string_lossy()
    );
    let versioned_path = path.with_file_name(filename);
    match query {
        Some(query) => format!("sqlite://{}?{query}", versioned_path.display()),
        None => format!("sqlite://{}", versioned_path.display()),
    }
}

pub fn register_sqlite_vec() {
    unsafe {
        libsqlite3_sys::sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            unsafe extern "C" fn(
                *mut libsqlite3_sys::sqlite3,
                *mut *mut i8,
                *const libsqlite3_sys::sqlite3_api_routines,
            ) -> i32,
        >(
            sqlite_vec::sqlite3_vec_init as *const ()
        )));
    }
}
