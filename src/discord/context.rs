use std::sync::Arc;

use sqlx::SqlitePool;

use crate::{
    chronicle::{config::app::Config, recording::recorder::RecorderManager, service::Chronicle},
    jester::{player::service::PlayerService, track::download::DownloadConfig},
    shutdown::ShutdownState,
};

// Defines user data; this is always available in the Serenity context of an invocation
pub struct Data {
    pub db_pool: SqlitePool,
    pub player: Arc<PlayerService>,
    pub recorder: RecorderManager,
    pub download_config: DownloadConfig,
    pub config: Config,
    pub chronicle: Arc<Chronicle>,
    pub shutdown: Arc<ShutdownState>,
}

impl Data {
    pub fn new(
        db_pool: SqlitePool,
        config: Config,
        chronicle: Arc<Chronicle>,
        recorder: RecorderManager,
        player: Arc<PlayerService>,
        shutdown: Arc<ShutdownState>,
    ) -> Self {
        let paths = config.paths.clone();
        let download_config = DownloadConfig::from(&paths);
        Self {
            db_pool,
            player,
            recorder,
            download_config,
            config,
            chronicle,
            shutdown,
        }
    }

    pub fn ensure_running(&self) -> Result<(), Error> {
        if self.shutdown.is_requested() {
            Err("The application is shutting down.".into())
        } else {
            Ok(())
        }
    }
}

pub type Error = Box<dyn std::error::Error + Send + Sync>;

pub type PoiseContext<'a> = poise::Context<'a, Data, Error>;
