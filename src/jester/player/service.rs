use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Weak,
        atomic::{AtomicU64, Ordering},
    },
};

use poise::serenity_prelude::{GuildId, UserId};
use songbird::{
    Call, Event, EventContext, EventHandler, TrackEvent,
    driver::Bitrate,
    input::{File as SongbirdFile, cached::Compressed},
    tracks::TrackHandle,
};
use tokio::sync::Mutex;
use tracing::{debug, error, info};

use crate::{
    discord::context::Error,
    jester::{
        player::queue::{
            GuildQueue, HistoryEntry, PlaybackItem, QueueEntry, QueueTransition, RepeatMode,
        },
        track::types::TrackInfo,
    },
};

struct ActivePlayback {
    id: u64,
    handle: TrackHandle,
}

#[derive(Clone, Debug)]
pub struct QueueSnapshot {
    pub current: Option<PlaybackItem>,
    pub upcoming: Vec<QueueEntry>,
    pub repeat_mode: RepeatMode,
}

pub struct PlayerService {
    queues: Mutex<HashMap<GuildId, GuildQueue>>,
    handles: Mutex<HashMap<GuildId, ActivePlayback>>,
    calls: Mutex<HashMap<GuildId, Arc<Mutex<Call>>>>,
    operation_lock: Mutex<()>,
    next_playback_id: AtomicU64,
    audio_dir: PathBuf,
}

impl PlayerService {
    pub fn new(audio_dir: PathBuf) -> Self {
        Self {
            queues: Mutex::new(HashMap::new()),
            handles: Mutex::new(HashMap::new()),
            calls: Mutex::new(HashMap::new()),
            operation_lock: Mutex::new(()),
            next_playback_id: AtomicU64::new(1),
            audio_dir,
        }
    }

    pub async fn play_now(
        self: &Arc<Self>,
        guild_id: GuildId,
        call: Arc<Mutex<Call>>,
        track: TrackInfo,
    ) -> Result<(), Error> {
        let _operation = self.operation_lock.lock().await;
        self.calls.lock().await.insert(guild_id, call.clone());
        self.stop_active(guild_id).await;
        let transition = self
            .queues
            .lock()
            .await
            .entry(guild_id)
            .or_default()
            .play_now(track);
        self.start_transition(guild_id, call, transition).await
    }

    pub async fn enqueue(
        self: &Arc<Self>,
        guild_id: GuildId,
        call: Arc<Mutex<Call>>,
        track: TrackInfo,
        requested_by: UserId,
        next: bool,
    ) -> Result<bool, Error> {
        let _operation = self.operation_lock.lock().await;
        self.calls.lock().await.insert(guild_id, call.clone());
        let transition = {
            let mut queues = self.queues.lock().await;
            let queue = queues.entry(guild_id).or_default();
            if next {
                queue.enqueue_next(track, Some(requested_by))
            } else {
                queue.enqueue(track, Some(requested_by))
            }
        };
        let started = transition.current.is_some();
        self.start_transition(guild_id, call, transition).await?;
        Ok(started)
    }

    pub async fn skip(self: &Arc<Self>, guild_id: GuildId) -> Result<TrackInfo, Error> {
        let _operation = self.operation_lock.lock().await;
        let call = self
            .calls
            .lock()
            .await
            .get(&guild_id)
            .cloned()
            .ok_or("No track is currently playing.")?;
        let transition = self
            .queues
            .lock()
            .await
            .entry(guild_id)
            .or_default()
            .skip()?;
        let next = transition
            .current
            .as_ref()
            .ok_or("No queued track is available to skip to.")?
            .track
            .clone();
        self.stop_active(guild_id).await;
        self.start_transition(guild_id, call, transition).await?;
        Ok(next)
    }

    pub async fn queue_snapshot(&self, guild_id: GuildId) -> QueueSnapshot {
        let queues = self.queues.lock().await;
        let queue = queues.get(&guild_id);
        QueueSnapshot {
            current: queue.and_then(|q| q.current().cloned()),
            upcoming: queue
                .map(|q| q.upcoming().iter().cloned().collect())
                .unwrap_or_default(),
            repeat_mode: queue.map(GuildQueue::repeat_mode).unwrap_or_default(),
        }
    }

    pub async fn history(&self, guild_id: GuildId) -> Vec<HistoryEntry> {
        self.queues
            .lock()
            .await
            .get(&guild_id)
            .map(|queue| queue.history().iter().cloned().collect())
            .unwrap_or_default()
    }

    pub async fn remove_queue_entry(
        &self,
        guild_id: GuildId,
        position: usize,
    ) -> Result<TrackInfo, Error> {
        Ok(self
            .queues
            .lock()
            .await
            .entry(guild_id)
            .or_default()
            .remove(position)?
            .track)
    }
    pub async fn move_queue_entry(
        &self,
        guild_id: GuildId,
        from: usize,
        to: usize,
    ) -> Result<(), Error> {
        self.queues
            .lock()
            .await
            .entry(guild_id)
            .or_default()
            .move_entry(from, to)?;
        Ok(())
    }
    pub async fn clear_queue(&self, guild_id: GuildId) {
        self.queues
            .lock()
            .await
            .entry(guild_id)
            .or_default()
            .clear_upcoming();
    }
    pub async fn shuffle_queue(&self, guild_id: GuildId) {
        use rand::seq::SliceRandom;
        self.queues
            .lock()
            .await
            .entry(guild_id)
            .or_default()
            .shuffle_with(|entries| entries.shuffle(&mut rand::rng()));
    }
    pub async fn set_repeat_mode(&self, guild_id: GuildId, mode: RepeatMode) {
        self.queues
            .lock()
            .await
            .entry(guild_id)
            .or_default()
            .set_repeat_mode(mode);
    }

    pub async fn pause(&self, guild_id: GuildId) -> Result<bool, Error> {
        let handles = self.handles.lock().await;
        let active = handles
            .get(&guild_id)
            .ok_or("No track is currently playing.")?;
        let state = active.handle.get_info().await?;
        if state.playing == songbird::tracks::PlayMode::Play {
            active.handle.pause()?;
            Ok(false)
        } else {
            active.handle.play()?;
            Ok(true)
        }
    }

    pub async fn get_now_playing(&self, guild_id: GuildId) -> Option<TrackInfo> {
        self.queue_snapshot(guild_id)
            .await
            .current
            .map(|item| item.track)
    }

    pub async fn clear_now_playing(&self, guild_id: GuildId) {
        let _operation = self.operation_lock.lock().await;
        self.stop_active(guild_id).await;
        self.calls.lock().await.remove(&guild_id);
        self.queues.lock().await.remove(&guild_id);
    }

    async fn start_transition(
        self: &Arc<Self>,
        guild_id: GuildId,
        call: Arc<Mutex<Call>>,
        transition: QueueTransition,
    ) -> Result<(), Error> {
        if let Some(item) = transition.current {
            self.start_item(guild_id, call, item).await?;
            info!(?guild_id, "Started playback");
        }
        Ok(())
    }

    async fn start_item(
        self: &Arc<Self>,
        guild_id: GuildId,
        call: Arc<Mutex<Call>>,
        item: PlaybackItem,
    ) -> Result<(), Error> {
        let path = self
            .audio_dir
            .join(format!("{}.mp3", item.track.id.as_str()));
        let source =
            Compressed::new(SongbirdFile::new(path).into(), Bitrate::Bits(128_000)).await?;
        let _ = source.raw.spawn_loader();
        let playback_id = self.next_playback_id.fetch_add(1, Ordering::Relaxed);
        let handle = call.lock().await.play_only_input(source.into());
        handle.add_event(
            Event::Track(TrackEvent::End),
            TrackEndHandler {
                player: Arc::downgrade(self),
                guild_id,
                playback_id,
            },
        )?;
        self.handles.lock().await.insert(
            guild_id,
            ActivePlayback {
                id: playback_id,
                handle,
            },
        );
        Ok(())
    }

    async fn stop_active(&self, guild_id: GuildId) {
        if let Some(active) = self.handles.lock().await.remove(&guild_id)
            && let Err(error) = active.handle.stop()
        {
            debug!(?guild_id, %error, "Track was already stopped");
        }
    }

    async fn handle_track_end(
        self: &Arc<Self>,
        guild_id: GuildId,
        playback_id: u64,
    ) -> Result<(), Error> {
        let _operation = self.operation_lock.lock().await;
        let is_current = self
            .handles
            .lock()
            .await
            .get(&guild_id)
            .is_some_and(|active| active.id == playback_id);
        if !is_current {
            return Ok(());
        }
        self.handles.lock().await.remove(&guild_id);
        let transition = match self.queues.lock().await.get_mut(&guild_id) {
            Some(queue) => queue.complete_current(),
            None => return Ok(()),
        };
        if let Some(call) = self.calls.lock().await.get(&guild_id).cloned() {
            self.start_transition(guild_id, call, transition).await?;
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
