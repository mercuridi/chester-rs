use sqlx::SqlitePool;
use tokio::sync::RwLock;
use poise::serenity_prelude::GuildId;
use songbird::tracks::TrackHandle;
use std::collections::HashMap;

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
    pub db_pool: SqlitePool, // Database pool
    pub track_handles: RwLock<HashMap<GuildId, TrackHandle>>, // Track handles for each guild
    pub track_metadata: RwLock<HashMap<GuildId, String>>, // Map of GuildId to currently playing track ID
}
impl Data {
    pub fn new(db_pool: SqlitePool) -> Self {
        Self {
            db_pool,
            track_handles: RwLock::new(HashMap::new()),
            track_metadata: RwLock::new(HashMap::new()),
        }
    }
}

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Context<'a> = poise::Context<'a, Data, Error>;
