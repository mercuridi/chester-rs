use anyhow::{Result, anyhow};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Weak,
        atomic::{AtomicU64, Ordering},
    },
};

use poise::serenity_prelude::GuildId;
use songbird::{
    Call, Event, EventContext, EventHandler, TrackEvent,
    driver::Bitrate,
    input::{File as SongbirdFile, Input, cached::Compressed},
    tracks::TrackHandle,
};
use tokio::sync::Mutex;
use tracing::{debug, error, info};

use super::queue::{
    GuildQueue, HistoryEntry, PlaybackItem, QueueEntry, QueueTransition, RepeatMode,
};
use crate::jester::track::TrackInfo;

struct ActivePlayback {
    id: u64,
    handle: TrackHandle,
}

#[derive(Default)]
struct GuildPlayerState {
    queue: GuildQueue,
    active: Option<ActivePlayback>,
    call: Option<Arc<Mutex<Call>>>,
}

#[derive(Clone, Debug)]
pub struct QueueSnapshot {
    pub current: Option<PlaybackItem>,
    pub upcoming: Vec<QueueEntry>,
    pub repeat_mode: RepeatMode,
}

#[async_trait::async_trait]
trait PlaybackAdapter: Send + Sync {
    async fn start(
        &self,
        guild_id: GuildId,
        call: Arc<Mutex<Call>>,
        item: &PlaybackItem,
        player: Weak<PlayerService>,
        playback_id: u64,
    ) -> Result<TrackHandle>;
    fn stop(&self, guild_id: GuildId, handle: TrackHandle);
    async fn toggle_pause(&self, handle: TrackHandle) -> Result<bool>;
}

struct SongbirdAdapter {
    audio_dir: PathBuf,
}

#[async_trait::async_trait]
impl PlaybackAdapter for SongbirdAdapter {
    async fn start(
        &self,
        guild_id: GuildId,
        call: Arc<Mutex<Call>>,
        item: &PlaybackItem,
        player: Weak<PlayerService>,
        playback_id: u64,
    ) -> Result<TrackHandle> {
        let path = self
            .audio_dir
            .join(format!("{}.mp3", item.track.id.as_str()));
        let source =
            Compressed::new(SongbirdFile::new(path).into(), Bitrate::Bits(128_000)).await?;
        let _ = source.raw.spawn_loader();
        let handle = call.lock().await.play_only_input(Input::from(source));
        if let Err(error) = handle.add_event(
            Event::Track(TrackEvent::End),
            TrackEndHandler {
                player,
                guild_id,
                playback_id,
            },
        ) {
            if let Err(stop_error) = handle.stop() {
                debug!(?guild_id, %stop_error, "Failed to clean up a track whose event registration failed");
            }
            return Err(error.into());
        }
        Ok(handle)
    }

    fn stop(&self, guild_id: GuildId, handle: TrackHandle) {
        if let Err(error) = handle.stop() {
            debug!(?guild_id, %error, "Track was already stopped");
        }
    }

    async fn toggle_pause(&self, handle: TrackHandle) -> Result<bool> {
        let state = handle.get_info().await?;
        if state.playing == songbird::tracks::PlayMode::Play {
            handle.pause()?;
            Ok(false)
        } else {
            handle.play()?;
            Ok(true)
        }
    }
}

pub struct PlayerService {
    players: Mutex<HashMap<GuildId, Arc<Mutex<GuildPlayerState>>>>,
    adapter: Arc<dyn PlaybackAdapter>,
    next_playback_id: AtomicU64,
}

impl PlayerService {
    pub fn new(audio_dir: PathBuf) -> Self {
        Self::with_adapter(Arc::new(SongbirdAdapter { audio_dir }))
    }

    fn with_adapter(adapter: Arc<dyn PlaybackAdapter>) -> Self {
        Self {
            players: Mutex::new(HashMap::new()),
            adapter,
            next_playback_id: AtomicU64::new(1),
        }
    }

    async fn player_state(&self, guild_id: GuildId) -> Arc<Mutex<GuildPlayerState>> {
        let mut players = self.players.lock().await;
        players
            .entry(guild_id)
            .or_insert_with(|| Arc::new(Mutex::new(GuildPlayerState::default())))
            .clone()
    }

    pub async fn play_now(
        self: &Arc<Self>,
        guild_id: GuildId,
        call: Arc<Mutex<Call>>,
        track: TrackInfo,
    ) -> Result<()> {
        let state = self.player_state(guild_id).await;
        let mut state = state.lock().await;
        state.call = Some(call);
        self.stop_active(guild_id, &mut state);
        let transition = state.queue.play_now(track);
        self.start_transition(guild_id, &mut state, transition)
            .await
    }

    pub async fn enqueue(
        self: &Arc<Self>,
        guild_id: GuildId,
        call: Arc<Mutex<Call>>,
        track: TrackInfo,
        next: bool,
    ) -> Result<bool> {
        let state = self.player_state(guild_id).await;
        let mut state = state.lock().await;
        state.call = Some(call);
        let transition = if next {
            state.queue.enqueue_next(track)
        } else {
            state.queue.enqueue(track)
        };
        let started = transition.current.is_some();
        self.start_transition(guild_id, &mut state, transition)
            .await?;
        Ok(started)
    }

    pub async fn skip(self: &Arc<Self>, guild_id: GuildId) -> Result<TrackInfo> {
        let state = self.player_state(guild_id).await;
        let mut state = state.lock().await;
        let call = state
            .call
            .clone()
            .ok_or_else(|| anyhow!("No track is currently playing."))?;
        let transition = state.queue.skip()?;
        let next = transition
            .current
            .as_ref()
            .ok_or_else(|| anyhow!("No queued track is available to skip to."))?
            .track
            .clone();
        self.stop_active(guild_id, &mut state);
        state.call = Some(call);
        self.start_transition(guild_id, &mut state, transition)
            .await?;
        Ok(next)
    }

    pub async fn queue_snapshot(&self, guild_id: GuildId) -> QueueSnapshot {
        let state = self.player_state(guild_id).await;
        let state = state.lock().await;
        QueueSnapshot {
            current: state.queue.current().cloned(),
            upcoming: state.queue.upcoming().iter().cloned().collect(),
            repeat_mode: state.queue.repeat_mode(),
        }
    }

    pub async fn history(&self, guild_id: GuildId) -> Vec<HistoryEntry> {
        let state = self.player_state(guild_id).await;
        state.lock().await.queue.history().iter().cloned().collect()
    }

    pub async fn remove_queue_entry(
        &self,
        guild_id: GuildId,
        position: usize,
    ) -> Result<TrackInfo> {
        let state = self.player_state(guild_id).await;
        Ok(state.lock().await.queue.remove(position)?.track)
    }

    pub async fn move_queue_entry(&self, guild_id: GuildId, from: usize, to: usize) -> Result<()> {
        let state = self.player_state(guild_id).await;
        state.lock().await.queue.move_entry(from, to)?;
        Ok(())
    }

    pub async fn clear_queue(&self, guild_id: GuildId) {
        let state = self.player_state(guild_id).await;
        state.lock().await.queue.clear_upcoming();
    }

    pub async fn shuffle_queue(&self, guild_id: GuildId) {
        use rand::seq::SliceRandom;
        let state = self.player_state(guild_id).await;
        state
            .lock()
            .await
            .queue
            .shuffle_with(|entries| entries.shuffle(&mut rand::rng()));
    }

    pub async fn set_repeat_mode(&self, guild_id: GuildId, mode: RepeatMode) {
        let state = self.player_state(guild_id).await;
        state.lock().await.queue.set_repeat_mode(mode);
    }

    pub async fn pause(&self, guild_id: GuildId) -> Result<bool> {
        let state = self.player_state(guild_id).await;
        let state = state.lock().await;
        let active = state
            .active
            .as_ref()
            .ok_or_else(|| anyhow!("No track is currently playing."))?;
        self.adapter.toggle_pause(active.handle.clone()).await
    }

    pub async fn get_now_playing(&self, guild_id: GuildId) -> Option<TrackInfo> {
        self.queue_snapshot(guild_id)
            .await
            .current
            .map(|item| item.track)
    }

    pub async fn clear_now_playing(&self, guild_id: GuildId) {
        let state = self.player_state(guild_id).await;
        let mut state = state.lock().await;
        self.stop_active(guild_id, &mut state);
        state.call = None;
        state.queue = GuildQueue::default();
    }

    pub async fn shutdown(&self) {
        let states: Vec<_> = self
            .players
            .lock()
            .await
            .iter()
            .map(|(guild_id, state)| (*guild_id, state.clone()))
            .collect();
        for (guild_id, state) in states {
            let mut state = state.lock().await;
            self.stop_active(guild_id, &mut state);
            state.call = None;
            state.queue = GuildQueue::default();
        }
    }

    async fn start_transition(
        self: &Arc<Self>,
        guild_id: GuildId,
        state: &mut GuildPlayerState,
        transition: QueueTransition,
    ) -> Result<()> {
        let mut next = transition.current;
        let mut first_error = None;
        while let Some(item) = next {
            let playback_id = self.next_playback_id.fetch_add(1, Ordering::Relaxed);
            let call = state
                .call
                .clone()
                .ok_or_else(|| anyhow!("No voice call is available."))?;
            match self
                .adapter
                .start(guild_id, call, &item, Arc::downgrade(self), playback_id)
                .await
            {
                Ok(handle) => {
                    state.active = Some(ActivePlayback {
                        id: playback_id,
                        handle,
                    });
                    info!(?guild_id, "Started playback");
                    return first_error.map_or(Ok(()), Err);
                }
                Err(error) => {
                    error!(?guild_id, %error, "Failed to start playback item");
                    first_error.get_or_insert(error);
                    next = state.queue.fail_current().current;
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    fn stop_active(&self, guild_id: GuildId, state: &mut GuildPlayerState) {
        if let Some(active) = state.active.take() {
            self.adapter.stop(guild_id, active.handle);
        }
    }

    async fn handle_track_end(self: &Arc<Self>, guild_id: GuildId, playback_id: u64) -> Result<()> {
        let state = self.player_state(guild_id).await;
        let mut state = state.lock().await;
        if state
            .active
            .as_ref()
            .is_none_or(|active| active.id != playback_id)
        {
            return Ok(());
        }
        state.active = None;
        let transition = state.queue.complete_current();
        if state.call.is_some() {
            self.start_transition(guild_id, &mut state, transition)
                .await?;
        }
        Ok(())
    }
}

struct TrackEndHandler {
    player: Weak<PlayerService>,
    guild_id: GuildId,
    playback_id: u64,
}

#[async_trait::async_trait]
impl EventHandler for TrackEndHandler {
    async fn act(&self, _: &EventContext<'_>) -> Option<Event> {
        if let Some(player) = self.player.upgrade()
            && let Err(error) = player
                .handle_track_end(self.guild_id, self.playback_id)
                .await
        {
            error!(?self.guild_id, %error, "Failed to advance playback queue");
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jester::track::{TrackInfo, VideoId};
    use anyhow::anyhow;
    use poise::serenity_prelude::{GuildId, UserId};
    use songbird::Call;
    use std::{
        path::PathBuf,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };

    fn track(id: &str) -> TrackInfo {
        TrackInfo {
            id: VideoId::from(id),
            title: id.into(),
            artist: None,
            origin: None,
        }
    }
    fn call(guild_id: GuildId) -> Arc<tokio::sync::Mutex<Call>> {
        Arc::new(tokio::sync::Mutex::new(Call::standalone(
            guild_id,
            UserId::new(1),
        )))
    }

    struct DelayedFailureAdapter {
        slow_guild: GuildId,
        fast_started: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl super::PlaybackAdapter for DelayedFailureAdapter {
        async fn start(
            &self,
            guild_id: GuildId,
            _: Arc<Mutex<Call>>,
            _: &super::PlaybackItem,
            _: Weak<super::PlayerService>,
            _: u64,
        ) -> anyhow::Result<TrackHandle> {
            if guild_id == self.slow_guild {
                tokio::time::sleep(Duration::from_millis(150)).await;
            } else {
                self.fast_started.store(true, Ordering::Release);
            }
            Err(anyhow!("test startup failure"))
        }

        fn stop(&self, _: GuildId, _: TrackHandle) {}

        async fn toggle_pause(&self, _: TrackHandle) -> anyhow::Result<bool> {
            Err(anyhow!("no test playback"))
        }
    }

    #[tokio::test]
    async fn play_now_does_not_leave_a_missing_file_selected() {
        let player = Arc::new(PlayerService::new(PathBuf::from("/does/not/exist")));
        let guild_id = GuildId::new(1);
        assert!(
            player
                .play_now(guild_id, call(guild_id), track("missing"))
                .await
                .is_err()
        );
        assert!(player.queue_snapshot(guild_id).await.current.is_none());
        assert!(
            player
                .history(guild_id)
                .await
                .iter()
                .any(|entry| entry.outcome == crate::jester::player::HistoryOutcome::Failed)
        );
    }

    #[tokio::test]
    async fn enqueue_does_not_leave_a_missing_first_item_selected() {
        let player = Arc::new(PlayerService::new(PathBuf::from("/does/not/exist")));
        let guild_id = GuildId::new(2);
        assert!(
            player
                .enqueue(guild_id, call(guild_id), track("missing"), false)
                .await
                .is_err()
        );
        assert!(player.queue_snapshot(guild_id).await.current.is_none());
    }

    #[tokio::test]
    async fn startup_in_one_guild_does_not_block_another_guild() {
        let slow_guild = GuildId::new(3);
        let fast_guild = GuildId::new(4);
        let fast_started = Arc::new(AtomicBool::new(false));
        let player = Arc::new(PlayerService::with_adapter(Arc::new(
            DelayedFailureAdapter {
                slow_guild,
                fast_started: fast_started.clone(),
            },
        )));
        let slow_player = player.clone();
        let slow = tokio::spawn(async move {
            slow_player
                .play_now(slow_guild, call(slow_guild), track("slow"))
                .await
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        let fast = player
            .play_now(fast_guild, call(fast_guild), track("fast"))
            .await;

        let slow = slow.await;
        assert!(slow.is_ok_and(|result| result.is_err()));
        assert!(fast.is_err());
        assert!(fast_started.load(Ordering::Acquire));
        assert!(player.queue_snapshot(fast_guild).await.current.is_none());
    }
}
