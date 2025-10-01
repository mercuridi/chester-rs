use serde::{Serialize, Deserialize};
use tokio::sync::{Mutex, RwLock};
use poise::serenity_prelude::GuildId;
use songbird::tracks::TrackHandle;
use std::{collections::HashMap, sync::Arc};
use rusqlite::Connection;

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Context<'a> = poise::Context<'a, Data, Error>;

// Defines user data; this is always available in the Serenity context of an invocation
pub struct Data {
    pub db_connection: Arc<Mutex<Connection>>, // Thread-safe database connection
    pub track_handles: RwLock<HashMap<GuildId, TrackHandle>>
}

#[derive(Serialize, Deserialize, Clone)]
pub struct TrackInfo {
        pub id: String,
        pub upload_date: String,
        pub yt_title: String,
        pub yt_channel: String,
        pub track_title: String,
        pub track_artist: String,
        pub track_origin: String,
        pub tags: Vec<String>,
}