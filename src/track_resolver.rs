use crate::definitions::{Error, TrackInfo, VideoId};
use crate::library::get_youtube_id;
use crate::cmd_management::download_track;
use sqlx::SqlitePool;

pub async fn lookup_track(
    db_pool: &SqlitePool,
    video_id: &VideoId,
) -> Result<Option<TrackInfo>, Error> {
    let result: Option<(String, String, String)> = sqlx::query_as(
        "SELECT tracks.track_title,
                artists.artist,
                origins.origin
         FROM tracks
         LEFT JOIN artists ON tracks.artist_id = artists.id
         LEFT JOIN origins ON tracks.origin_id = origins.id
         WHERE tracks.id = ?1"
    )
    .bind(video_id.as_str())
    .fetch_optional(db_pool)
    .await?;

    Ok(result.map(|(title, artist, origin)| TrackInfo {
        id: video_id.clone(),
        title,
        artist,
        origin,
    }))
}

pub fn normalise_track_input(
    input: &str,
) -> VideoId {
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

    // 1. Try DB first
    if let Some(track) =
        lookup_track(db_pool, &video_id).await?
    {
        return Ok(track);
    }

    // 2. Download fallback (NO ctx, NO UI)
    let track = download_track(
        db_pool,
        input,
        None,
        None,
        None,
    )
    .await?;

    Ok(track)
}