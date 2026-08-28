use crate::{
    discord::context::Error,
    jester::db::repository::lookup_track,
    jester::track::{
        download::download_track,
        types::{TrackInfo, VideoId},
        youtube::get_youtube_id,
    },
};
use sqlx::SqlitePool;
use tracing::{debug, info, instrument};

#[instrument]
pub fn normalise_track_input(input: &str) -> VideoId {
    VideoId::from(get_youtube_id(input).unwrap_or_else(|| input.to_string()))
}

#[instrument(skip(db_pool), fields(input = %input))]
pub async fn resolve_track(db_pool: &SqlitePool, input: String) -> Result<TrackInfo, Error> {
    let video_id = normalise_track_input(&input);
    debug!(track_id = %video_id.as_str(), "Resolving track");

    if let Some(track) = lookup_track(db_pool, &video_id).await? {
        info!(track_id = %video_id.as_str(), "Resolved track from library");
        return Ok(track);
    }

    info!(track_id = %video_id.as_str(), "Track is not in library; downloading");
    download_track(db_pool, input, None, None, None).await
}
