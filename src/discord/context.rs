use std::sync::Arc;

use sqlx::SqlitePool;

use crate::{
    chronicle::{config::app::Config, recording::recorder::RecorderManager, service::Chronicle},
    jester::{player::service::PlayerService, track::download::DownloadConfig},
};

// Defines user data; this is always available in the Serenity context of an invocation
pub struct Data {
    pub db_pool: SqlitePool,
    pub player: Arc<PlayerService>,
    pub recorder: RecorderManager,
    pub download_config: DownloadConfig,
    pub config: Config,
    pub chronicle: Chronicle,
}

impl Data {
    pub fn new(db_pool: SqlitePool, config: Config, chronicle: Chronicle) -> Self {
        let paths = config.paths.clone();
        let download_config = DownloadConfig::from(&paths);
        Self {
            db_pool,
            player: Arc::new(PlayerService::new(paths.audio_dir)),
            recorder: RecorderManager::new(paths.recordings_dir),
            download_config,
            config,
            chronicle,
        }
    }
}

pub type Error = Box<dyn std::error::Error + Send + Sync>;

pub type PoiseContext<'a> = poise::Context<'a, Data, Error>;
