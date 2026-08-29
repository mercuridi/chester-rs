use poise::serenity_prelude::GuildId;
use songbird::driver::Bitrate;
use songbird::input::File as SongbirdFile;
use songbird::input::cached::Compressed;
use songbird::{Call, tracks::LoopState};
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::sync::{Mutex, RwLock};

use crate::{
    constants::AUDIO_DIR,
    discord::context::Error,
    jester::track::types::{NowPlaying, TrackInfo},
};
use tracing::{debug, info, instrument};

pub struct PlayerService {
    now_playing: RwLock<HashMap<GuildId, NowPlaying>>,
}

impl PlayerService {
    pub fn new() -> Self {
        Self {
            now_playing: RwLock::new(HashMap::new()),
        }
    }

    #[instrument(skip(self, call), fields(track_id = ?track_info.id, title = %track_info.title))]
    pub async fn play(
        &self,
        guild_id: GuildId,
        call: Arc<Mutex<Call>>,
        track_info: TrackInfo,
    ) -> Result<(), Error> {
        let mut handler = call.lock().await;

        let track_path = PathBuf::from(AUDIO_DIR).join(format!("{}.mp3", track_info.id.as_str()));

        let song_src =
            Compressed::new(SongbirdFile::new(track_path).into(), Bitrate::Bits(128_000)).await?;

        let _ = song_src.raw.spawn_loader();

        let track_handle = handler.play_only_input(song_src.into());
        let () = track_handle.enable_loop()?;

        let mut state = self.now_playing.write().await;

        state.insert(
            guild_id,
            NowPlaying {
                track: track_info,
                handle: track_handle,
            },
        );

        info!(?guild_id, "Started playback");

        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn pause(&self, guild_id: GuildId) -> Result<bool, Error> {
        let state = self.now_playing.read().await;
        let now = state
            .get(&guild_id)
            .ok_or("No track is currently playing.")?;

        let info = now.handle.get_info().await?;
        if info.playing == songbird::tracks::PlayMode::Play {
            now.handle.pause()?;
            info!(?guild_id, "Paused playback");
            Ok(false) // is now paused
        } else {
            now.handle.play()?;
            info!(?guild_id, "Resumed playback");
            Ok(true) // is now playing
        }
    }

    #[instrument(skip(self))]
    pub async fn toggle_loop(&self, guild_id: GuildId) -> Result<bool, Error> {
        let state = self.now_playing.read().await;
        let now = state
            .get(&guild_id)
            .ok_or("No track is currently playing.")?;

        let info = now.handle.get_info().await?;
        match info.loops {
            LoopState::Infinite => {
                now.handle.disable_loop()?;
                info!(?guild_id, enabled = false, "Updated playback loop");
                Ok(false) // looping now disabled
            }
            LoopState::Finite(_) => {
                now.handle.enable_loop()?;
                info!(?guild_id, enabled = true, "Updated playback loop");
                Ok(true) // looping now enabled
            }
        }
    }

    pub async fn get_now_playing(&self, guild_id: GuildId) -> Option<TrackInfo> {
        self.now_playing
            .read()
            .await
            .get(&guild_id)
            .map(|np| np.track.clone())
    }

    pub async fn clear_now_playing(&self, guild_id: GuildId) {
        debug!(?guild_id, "Clearing now-playing state");
        self.now_playing.write().await.remove(&guild_id);
    }

    pub async fn require_now_playing(&self, guild_id: GuildId) -> Result<TrackInfo, Error> {
        self.get_now_playing(guild_id)
            .await
            .ok_or_else(|| "No track is currently playing.".into())
    }
}
