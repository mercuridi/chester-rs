use crate::definitions::{Error, TrackInfo, VideoId};
use crate::utils::context::get_youtube_id;
use crate::utils::downloader::download_track;
use crate::db::repository::lookup_track;
use sqlx::SqlitePool;

pub fn normalise_track_input(input: &str) -> VideoId {
    VideoId::from(
        get_youtube_id(input)
            .unwrap_or_else(|| input.to_string())
    )
}

pub async fn resolve_track(
    db_pool: &SqlitePool,
    input: String,
) -> Result<TrackInfo, Error> {
    let video_id = normalise_track_input(&input);

    if let Some(track) = lookup_track(db_pool, &video_id).await? {
        return Ok(track);
    }

    download_track(db_pool, input, None, None, None).await
}