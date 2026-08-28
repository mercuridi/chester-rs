use sqlx::SqlitePool;

use crate::{
    chronicle::{config::Config, recording::recorder::RecorderManager, service::Chronicle},
    jester::player::service::PlayerService,
};

// Defines user data; this is always available in the Serenity context of an invocation
pub struct Data {
    pub db_pool: SqlitePool,
    pub player: PlayerService,
    pub recorder: RecorderManager,
    pub config: Config,
    pub chronicle: Chronicle,
}

impl Data {
    pub fn new(db_pool: SqlitePool, config: Config, chronicle: Chronicle) -> Self {
        Self {
            db_pool,
            player: PlayerService::new(),
            recorder: RecorderManager::new(),
            config,
            chronicle,
        }
    }
}

pub type Error = Box<dyn std::error::Error + Send + Sync>;

pub type PoiseContext<'a> = poise::Context<'a, Data, Error>;
