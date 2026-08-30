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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::PlayerState;
    use crate::jester::track::types::{TrackInfo, VideoId};
    use poise::serenity_prelude::GuildId;

    fn track(title: &str) -> TrackInfo {
        TrackInfo {
            id: VideoId::from(title),
            title: title.into(),
            artist: "artist".into(),
            origin: "origin".into(),
        }
    }

    #[tokio::test]
    async fn state_is_empty_by_default() {
        assert!(PlayerState::default().get(GuildId::new(1)).await.is_none());
    }

    #[tokio::test]
    async fn state_tracks_guilds_independently() {
        let state = PlayerState::default();
        state.set(GuildId::new(1), track("one")).await;
        state.set(GuildId::new(2), track("two")).await;

        assert_eq!(state.get(GuildId::new(1)).await.unwrap().title, "one");
        assert_eq!(state.get(GuildId::new(2)).await.unwrap().title, "two");
    }

    #[tokio::test]
    async fn setting_a_guild_replaces_its_track() {
        let state = PlayerState::default();
        state.set(GuildId::new(1), track("old")).await;
        state.set(GuildId::new(1), track("new")).await;
        assert_eq!(state.get(GuildId::new(1)).await.unwrap().title, "new");
    }

    #[tokio::test]
    async fn clearing_one_guild_does_not_clear_another() {
        let state = PlayerState::default();
        state.set(GuildId::new(1), track("one")).await;
        state.set(GuildId::new(2), track("two")).await;
        state.clear(GuildId::new(1)).await;

        assert!(state.get(GuildId::new(1)).await.is_none());
        assert_eq!(state.get(GuildId::new(2)).await.unwrap().title, "two");
    }
}
