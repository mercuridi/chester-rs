use sqlx::SqlitePool;

use crate::{chronicle::{config::Config, recorder::RecorderManager}, player::service::PlayerService};

// Defines user data; this is always available in the Serenity context of an invocation
pub struct Data {
    pub db_pool: SqlitePool,
    pub player: PlayerService,
    pub recorder: RecorderManager,
    pub config: Config,
}

impl Data {
    pub fn new(
        db_pool: SqlitePool,
        config: Config,
    ) -> Self {
        Self {
            db_pool,
            player: PlayerService::new(),
            recorder: RecorderManager::new(),
            config,
        }
    }
}


pub type Error = Box<dyn std::error::Error + Send + Sync>;

pub type PoiseContext<'a> = poise::Context<'a, Data, Error>;
