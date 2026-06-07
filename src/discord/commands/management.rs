use crate::definitions::{Error, MetadataKind, PoiseContext, TrackInfo, VideoId};
use crate::discord::autocomplete::{autocomplete_track, autocomplete_tag, autocomplete_origin, autocomplete_artist};
use crate::utils::downloader::download_track;
use crate::db::repository::{
    get_or_insert_metadata_id, require_track,
    delete_track_tags, insert_track_tag,
    update_track_title, update_track_artist, update_track_origin,
};

pub async fn download_direct(
    ctx: PoiseContext<'_>,
    yt_link: String,
    track_artist: Option<String>,
    track_origin: Option<String>,
    track_title: Option<String>,
) -> Result<TrackInfo, Error> {
    ctx.defer().await?;

    let track = download_track(
        &ctx.data().db_pool,
        yt_link,
        track_artist,
        track_origin,
        track_title,
    )
    .await?;

    ctx.say(format!(
        "File downloaded and added to the library: `{}`",
        track.title
    ))
    .await?;

    Ok(track)
}

/// Download a track from a YouTube link
#[poise::command(slash_command)]
pub async fn download(
    ctx: PoiseContext<'_>,
    #[description = "YouTube link to download from"]
    yt_link: String,
    #[description = "The actual artist of the track"]
    #[autocomplete = "autocomplete_artist"]
    track_artist: Option<String>,
    #[description = "The origin of the track (e.g., game/movie title)"]
    #[autocomplete = "autocomplete_origin"]
    track_origin: Option<String>,
    #[description = "The actual title of the track"]
    track_title: Option<String>,
) -> Result<(), Error> {
    download_direct(ctx, yt_link, track_artist, track_origin, track_title).await?;
    Ok(())
}

/// Reset a track's user-set metadata tags
#[poise::command(slash_command)]
pub async fn reset_tags(
    ctx: PoiseContext<'_>,
    #[description = "The track to reset the tags of"]
    #[autocomplete = "autocomplete_track"]
    track: String,
) -> Result<(), Error> {
    let db_pool = &ctx.data().db_pool;
    let info = require_track(db_pool, &VideoId::from(track)).await?;

    delete_track_tags(db_pool, &info.id).await?;

    ctx.say(format!("Reset tags for track `{}`", info.title)).await?;
    Ok(())
}

/// Add a new arbitrary tag to a track
#[poise::command(slash_command)]
pub async fn add_tag(
    ctx: PoiseContext<'_>,
    #[description = "The track to add a tag to"]
    #[autocomplete = "autocomplete_track"]
    track: String,
    #[description = "The tag to add"]
    #[autocomplete = "autocomplete_tag"]
    tag: String,
) -> Result<(), Error> {
    let db_pool = &ctx.data().db_pool;
    let info = require_track(db_pool, &VideoId::from(track)).await?;
    let tag_id = get_or_insert_metadata_id(db_pool, MetadataKind::Tag, &tag).await?;

    insert_track_tag(db_pool, &info.id, tag_id).await?;

    ctx.say(format!("Tag `{}` added to track `{}`", tag, info.title)).await?;
    Ok(())
}

/// Set a track's title, artist, or origin
#[poise::command(slash_command, subcommands("title", "artist", "origin"), subcommand_required)]
pub async fn set_metadata(
    _ctx: PoiseContext<'_>,
) -> Result<(), Error> {
    Ok(())
}

/// Set a track's title
#[poise::command(slash_command)]
pub async fn title(
    ctx: PoiseContext<'_>,
    #[description = "The track to adjust"]
    #[autocomplete = "autocomplete_track"]
    track: String,
    #[description = "The new title to give the track"]
    new_title: String,
) -> Result<(), Error> {
    let db_pool = &ctx.data().db_pool;
    let track_id = VideoId::from(track);
    let info = require_track(db_pool, &track_id).await?;

    update_track_title(db_pool, &info.id, &new_title).await?;

    ctx.say(format!(
        "Set new title `{}` for track `{}`",
        new_title,
        info.title
    ))
    .await?;
    Ok(())
}

/// Set a track's artist
#[poise::command(slash_command)]
pub async fn artist(
    ctx: PoiseContext<'_>,
    #[description = "The track to adjust"]
    #[autocomplete = "autocomplete_track"]
    track: String,
    #[description = "The new artist for the track"]
    #[autocomplete = "autocomplete_artist"]
    new_artist: String,
) -> Result<(), Error> {
    let db_pool = &ctx.data().db_pool;
    let info = require_track(db_pool, &VideoId::from(track)).await?;
    let artist_id = get_or_insert_metadata_id(db_pool, MetadataKind::Artist, &new_artist).await?;

    update_track_artist(db_pool, &info.id, artist_id).await?;

    ctx.say(format!(
        "Set new artist `{}` for track `{}`",
        new_artist,
        info.title
    ))
    .await?;
    Ok(())
}

/// Set a track's origin (e.g., game/movie title)
#[poise::command(slash_command)]
pub async fn origin(
    ctx: PoiseContext<'_>,
    #[description = "The track to adjust"]
    #[autocomplete = "autocomplete_track"]
    track: String,
    #[description = "The new origin for the track"]
    #[autocomplete = "autocomplete_origin"]
    new_origin: String,
) -> Result<(), Error> {
    let db_pool = &ctx.data().db_pool;
    let info = require_track(db_pool, &VideoId::from(track)).await?;
    let origin_id = get_or_insert_metadata_id(db_pool, MetadataKind::Origin, &new_origin).await?;

    update_track_origin(db_pool, &info.id, origin_id).await?;

    ctx.say(format!(
        "Set new origin `{}` for track `{}`",
        new_origin,
        info.title
    ))
    .await?;
    Ok(())
}