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
    pub now_playing: RwLock<HashMap<GuildId, NowPlaying>>,
}
impl Data {
    pub fn new(db_pool: SqlitePool) -> Self {
        Self {
            db_pool,
            now_playing: RwLock::new(HashMap::new())
        }
    }
}
pub struct NowPlaying {
    pub track: TrackInfo,
    pub handle: TrackHandle,
}

// Defines user data; this is always available in the Serenity context of an invocation

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Context<'a> = poise::Context<'a, Data, Error>;
