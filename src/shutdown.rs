use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use songbird::Songbird;
use sqlx::SqlitePool;

use crate::{
    chronicle::transcription::service::TranscriptionService,
    chronicle::{recording::RecorderManager, service::Chronicle},
    jester::player::service::PlayerService,
};

#[derive(Default)]
pub struct ShutdownState {
    requested: AtomicBool,
}

impl ShutdownState {
    pub fn request(&self) -> bool {
        !self.requested.swap(true, Ordering::AcqRel)
    }

    pub fn is_requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }
}

pub struct ShutdownCoordinator {
    pub state: Arc<ShutdownState>,
    recorder: RecorderManager,
    player: Arc<PlayerService>,
    chronicle: Arc<Chronicle>,
    pool: SqlitePool,
    songbird: Arc<Songbird>,
    transcription: TranscriptionService,
    timeout: Duration,
}

impl ShutdownCoordinator {
    pub fn new(
        recorder: RecorderManager,
        player: Arc<PlayerService>,
        chronicle: Arc<Chronicle>,
        pool: SqlitePool,
        songbird: Arc<Songbird>,
        transcription: TranscriptionService,
        timeout: Duration,
    ) -> Self {
        Self {
            state: Arc::new(ShutdownState::default()),
            recorder,
            player,
            chronicle,
            pool,
            songbird,
            transcription,
            timeout,
        }
    }

    pub async fn drain(&self) -> anyhow::Result<()> {
        if !self.state.request() {
            return Ok(());
        }

        tracing::info!(timeout = ?self.timeout, "Beginning coordinated shutdown");
        let result = tokio::time::timeout(self.timeout, self.drain_inner()).await;
        if let Ok(result) = result {
            result
        } else {
            tracing::error!("Shutdown drain timed out; incomplete recording state was preserved");
            Ok(())
        }
    }

    async fn drain_inner(&self) -> anyhow::Result<()> {
        let mut errors = Vec::new();

        if let Err(error) = self.recorder.drain().await {
            errors.push(format!("recordings: {error}"));
        }

        self.transcription.drain().await;

        self.player.shutdown().await;

        let guilds: Vec<_> = self.songbird.iter().map(|(guild_id, _)| guild_id).collect();
        for guild_id in guilds {
            if let Err(error) = self.songbird.remove(guild_id).await {
                errors.push(format!("voice {guild_id}: {error}"));
            }
        }

        if self.chronicle.is_llm_loaded().unwrap_or(false)
            && let Err(error) = self.chronicle.stop_llm().await
        {
            errors.push(format!("Chronicle model: {error}"));
        }

        self.pool.close().await;

        if errors.is_empty() {
            tracing::info!("Coordinated shutdown complete");
            Ok(())
        } else {
            Err(anyhow::anyhow!(errors.join("; ")))
        }
    }
}
