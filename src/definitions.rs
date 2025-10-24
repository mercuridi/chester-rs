use sqlx::SqlitePool;
use tokio::sync::RwLock;
use poise::serenity_prelude::GuildId;
use songbird::tracks::TrackHandle;
use std::collections::HashMap;

// Defines user data; this is always available in the Serenity context of an invocation
pub struct Data {
    pub db_pool: SqlitePool, // Database pool
    pub track_handles: RwLock<HashMap<GuildId, TrackHandle>>, // Track handles for each guild
    pub track_metadata: RwLock<HashMap<GuildId, String>>, // Map of GuildId to currently playing track ID
}

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Context<'a> = poise::Context<'a, Data, Error>;
