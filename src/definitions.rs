use sqlx::SqlitePool;
use tokio::sync::RwLock;
use poise::serenity_prelude::GuildId;
use songbird::tracks::TrackHandle;
use std::collections::HashMap;

// Defines user data; this is always available in the Serenity context of an invocation
pub struct Data {
    pub db_pool: SqlitePool, // Add the database pool here
    pub track_handles: RwLock<HashMap<GuildId, TrackHandle>>
}

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Context<'a> = poise::Context<'a, Data, Error>;
