use std::collections::HashMap;
use tokio::sync::RwLock;
use songbird::tracks::LoopState;
use songbird::input::File as SongbirdFile;
use songbird::input::cached::Compressed;
use songbird::driver::Bitrate;
use poise::serenity_prelude::{ChannelId, GuildId};

use crate::definitions::{Error, NowPlaying, TrackInfo};

pub struct PlayerService {
    now_playing: RwLock<HashMap<GuildId, NowPlaying>>,
}

impl PlayerService {
    pub fn new() -> Self {
        Self {
            now_playing: RwLock::new(HashMap::new()),
        }
    }

    pub async fn play(
        &self,
        guild_id: GuildId,
        vc_id: ChannelId,
        track_info: TrackInfo,
        serenity_ctx: &poise::serenity_prelude::Context,
    ) -> Result<(), Error> {
        let manager = songbird::get(serenity_ctx)
            .await
            .expect("Songbird was not initialized")
            .clone();

        manager.join(guild_id, vc_id).await?;

        let track_path = format!("audio/{}.mp3", track_info.id.as_str());

        let song_src = Compressed::new(
            SongbirdFile::new(track_path).into(),
            Bitrate::Bits(128_000),
        )
        .await
        .expect("An error occurred constructing the track source");

        let _ = song_src.raw.spawn_loader();

        if let Some(handler_lock) = manager.get(guild_id) {
            let mut handler = handler_lock.lock().await;
            let track_handle = handler.play_only_input(song_src.into());
            let _ = track_handle.enable_loop()?;

            let mut state = self.now_playing.write().await;
            state.insert(guild_id, NowPlaying {
                track: track_info,
                handle: track_handle,
            });
        }

        Ok(())
    }

    pub async fn pause(&self, guild_id: GuildId) -> Result<bool, Error> {
        let state = self.now_playing.read().await;
        let now = state.get(&guild_id)
            .ok_or("No track is currently playing.")?;

        let info = now.handle.get_info().await?;
        if info.playing == songbird::tracks::PlayMode::Play {
            now.handle.pause()?;
            Ok(false) // is now paused
        } else {
            now.handle.play()?;
            Ok(true) // is now playing
        }
    }

    pub async fn toggle_loop(&self, guild_id: GuildId) -> Result<bool, Error> {
        let state = self.now_playing.read().await;
        let now = state.get(&guild_id)
            .ok_or("No track is currently playing.")?;

        let info = now.handle.get_info().await?;
        match info.loops {
            LoopState::Infinite => {
                now.handle.disable_loop()?;
                Ok(false) // looping now disabled
            }
            LoopState::Finite(_) => {
                now.handle.enable_loop()?;
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

    pub async fn leave(&self, guild_id: GuildId, serenity_ctx: &poise::serenity_prelude::Context) -> Result<(), Error> {
        let manager = songbird::get(serenity_ctx)
            .await
            .expect("Songbird was not initialized")
            .clone();

        manager.remove(guild_id).await?;

        let mut state = self.now_playing.write().await;
        state.remove(&guild_id);

        Ok(())
    }
}