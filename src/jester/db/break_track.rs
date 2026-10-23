use anyhow::{Context, Result};
use serenity::all::UserId;
use sqlx::SqlitePool;

use crate::jester::track::{TrackInfo, VideoId};

pub async fn get_break_track(pool: &SqlitePool, user_id: UserId) -> Result<Option<TrackInfo>> {
    let id: Option<String> =
        sqlx::query_scalar("SELECT track_id FROM gm_break_tracks WHERE user_id = ?")
            .bind(user_id.get().to_string())
            .fetch_optional(pool)
            .await
            .context("Failed to load GM break track")?;
    match id {
        Some(id) => crate::jester::db::require_track(pool, &VideoId::from(id))
            .await
            .map(Some),
        None => Ok(None),
    }
}

pub async fn set_break_track(pool: &SqlitePool, user_id: UserId, track: &TrackInfo) -> Result<()> {
    sqlx::query("INSERT INTO gm_break_tracks (user_id, track_id) VALUES (?, ?) ON CONFLICT(user_id) DO UPDATE SET track_id = excluded.track_id")
        .bind(user_id.get().to_string())
        .bind(track.id.as_str())
        .execute(pool)
        .await
        .context("Failed to save GM break track")?;
    Ok(())
}
