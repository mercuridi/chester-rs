use sqlx::SqlitePool;
use songbird::tracks::TrackHandle;
use crate::player::service::PlayerService;

pub enum MetadataKind {
    Artist,
    Origin,
    Tag,
}

impl MetadataKind {
    pub fn select_sql(&self) -> &'static str {
        match self {
            MetadataKind::Artist => "SELECT id FROM artists WHERE artist = ?1",
            MetadataKind::Origin => "SELECT id FROM origins WHERE origin = ?1",
            MetadataKind::Tag    => "SELECT id FROM tags WHERE tag = ?1",
        }
    }

    pub fn insert_sql(&self) -> &'static str {
        match self {
            MetadataKind::Artist => "INSERT INTO artists (artist) VALUES (?1)",
            MetadataKind::Origin => "INSERT INTO origins (origin) VALUES (?1)",
            MetadataKind::Tag    => "INSERT INTO tags (tag) VALUES (?1)",
        }
    }
}
// Track Info unified struct
#[derive(Clone, Debug)]
pub struct TrackInfo {
    pub id: VideoId,
    pub title: String,
    pub artist: String,
    pub origin: String,
}

// Domain types - semantic safety to prevent mixing incompatible values
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct VideoId(pub String);

impl VideoId {
    pub fn new(id: String) -> Self {
        VideoId(id)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for VideoId {
    fn from(s: String) -> Self {
        VideoId(s)
    }
}

impl From<&str> for VideoId {
    fn from(s: &str) -> Self {
        VideoId(s.to_string())
    }
}

// Defines user data; this is always available in the Serenity context of an invocation
pub struct Data {
    pub db_pool: SqlitePool,
    pub player: PlayerService,
}

impl Data {
    pub fn new(db_pool: SqlitePool) -> Self {
        Self {
            db_pool,
            player: PlayerService::new(),
        }
    }
}

pub struct NowPlaying {
    pub track: TrackInfo,
    pub handle: TrackHandle,
}

// Defines user data; this is always available in the Serenity context of an invocation

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type PoiseContext<'a> = poise::Context<'a, Data, Error>;
