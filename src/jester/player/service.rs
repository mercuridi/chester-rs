use poise::serenity_prelude::GuildId;
use songbird::driver::Bitrate;
use songbird::input::File as SongbirdFile;
use songbird::input::cached::Compressed;
use songbird::{Call, tracks::LoopState};
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::sync::Mutex;

use crate::{
    discord::context::Error,
    jester::{player::PlayerState, track::types::TrackInfo},
};
use tracing::{debug, info, instrument};

pub struct PlayerService {
    state: PlayerState,
    handles: Mutex<HashMap<GuildId, songbird::tracks::TrackHandle>>,
    audio_dir: PathBuf,
}

impl PlayerService {
    pub fn new(audio_dir: PathBuf) -> Self {
        Self {
            state: PlayerState::default(),
            handles: Mutex::new(HashMap::new()),
            audio_dir,
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

        let track_path = self
            .audio_dir
            .join(format!("{}.mp3", track_info.id.as_str()));

        let song_src =
            Compressed::new(SongbirdFile::new(track_path).into(), Bitrate::Bits(128_000)).await?;

        let _ = song_src.raw.spawn_loader();

        let track_handle = handler.play_only_input(song_src.into());
        let () = track_handle.enable_loop()?;

        self.handles.lock().await.insert(guild_id, track_handle);
        self.state.set(guild_id, track_info).await;

        info!(?guild_id, "Started playback");

        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn pause(&self, guild_id: GuildId) -> Result<bool, Error> {
        let handles = self.handles.lock().await;
        let now = handles
            .get(&guild_id)
            .ok_or("No track is currently playing.")?;

        let info = now.get_info().await?;
        if info.playing == songbird::tracks::PlayMode::Play {
            now.pause()?;
            info!(?guild_id, "Paused playback");
            Ok(false) // is now paused
        } else {
            now.play()?;
            info!(?guild_id, "Resumed playback");
            Ok(true) // is now playing
        }
    }

    #[instrument(skip(self))]
    pub async fn toggle_loop(&self, guild_id: GuildId) -> Result<bool, Error> {
        let handles = self.handles.lock().await;
        let now = handles
            .get(&guild_id)
            .ok_or("No track is currently playing.")?;

        let info = now.get_info().await?;
        match info.loops {
            LoopState::Infinite => {
                now.disable_loop()?;
                info!(?guild_id, enabled = false, "Updated playback loop");
                Ok(false) // looping now disabled
            }
            LoopState::Finite(_) => {
                now.enable_loop()?;
                info!(?guild_id, enabled = true, "Updated playback loop");
                Ok(true) // looping now enabled
            }
        }
    }

    pub async fn get_now_playing(&self, guild_id: GuildId) -> Option<TrackInfo> {
        self.state.get(guild_id).await
    }

    pub async fn clear_now_playing(&self, guild_id: GuildId) {
        debug!(?guild_id, "Clearing now-playing state");
        self.handles.lock().await.remove(&guild_id);
        self.state.clear(guild_id).await;
    }

    pub async fn require_now_playing(&self, guild_id: GuildId) -> Result<TrackInfo, Error> {
        self.get_now_playing(guild_id)
            .await
            .ok_or_else(|| "No track is currently playing.".into())
    }
}
