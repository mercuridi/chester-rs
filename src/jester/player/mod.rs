pub mod service;

use crate::jester::track::types::TrackInfo;
use poise::serenity_prelude::GuildId;
use std::collections::HashMap;
use tokio::sync::RwLock;

/// Backend-independent player state.  This is intentionally free of Songbird types.
#[derive(Default)]
pub struct PlayerState {
    tracks: RwLock<HashMap<GuildId, TrackInfo>>,
}

impl PlayerState {
    pub async fn set(&self, guild_id: GuildId, track: TrackInfo) {
        self.tracks.write().await.insert(guild_id, track);
    }
    pub async fn get(&self, guild_id: GuildId) -> Option<TrackInfo> {
        self.tracks.read().await.get(&guild_id).cloned()
    }
    pub async fn clear(&self, guild_id: GuildId) {
        self.tracks.write().await.remove(&guild_id);
    }
}
