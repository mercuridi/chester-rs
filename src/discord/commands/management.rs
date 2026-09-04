use crate::discord::autocomplete::{
    autocomplete_artist, autocomplete_environment, autocomplete_function,
    autocomplete_incomplete_track, autocomplete_intensity, autocomplete_mood, autocomplete_origin,
    autocomplete_texture, autocomplete_track,
};
use crate::discord::context::{Error, PoiseContext};
use crate::jester::db::metadata::MetadataKind;
use crate::jester::db::repository::{
    clear_track_taxonomy, get_or_insert_metadata_id, insert_track_environment, insert_track_label,
    insert_track_texture, require_track, set_track_taxonomy, update_track_artist,
    update_track_origin, update_track_title,
};
use crate::jester::db::taxonomy::{
    ENVIRONMENTS, FUNCTIONS, INTENSITIES, MOODS, TEXTURES, require_value,
};
use crate::jester::track::download::download_track;
use crate::jester::track::types::{TrackInfo, VideoId};

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

/// Download a track from a `YouTube` link
#[poise::command(slash_command)]
pub async fn download(
    ctx: PoiseContext<'_>,
    #[description = "YouTube link to download from"] yt_link: String,
    #[description = "The actual artist of the track"]
    #[autocomplete = "autocomplete_artist"]
    track_artist: Option<String>,
    #[description = "The origin of the track (e.g., game/movie title)"]
    #[autocomplete = "autocomplete_origin"]
    track_origin: Option<String>,
    #[description = "The actual title of the track"] track_title: Option<String>,
) -> Result<(), Error> {
    download_direct(ctx, yt_link, track_artist, track_origin, track_title).await?;
    Ok(())
}

/// Clear a track's taxonomy, textures, and custom labels
#[poise::command(slash_command)]
pub async fn reset_taxonomy(
    ctx: PoiseContext<'_>,
    #[description = "The track to reset the tags of"]
    #[autocomplete = "autocomplete_track"]
    track: String,
) -> Result<(), Error> {
    let db_pool = &ctx.data().db_pool;
    let info = require_track(db_pool, &VideoId::from(track)).await?;

    clear_track_taxonomy(db_pool, &info.id).await?;

    ctx.say(format!("Reset taxonomy for track `{}`", info.title))
        .await?;
    Ok(())
}

/// Set a track's controlled playlist taxonomy
#[poise::command(slash_command)]
pub async fn set_taxonomy(
    ctx: PoiseContext<'_>,
    #[description = "The track to add a tag to"]
    #[autocomplete = "autocomplete_track"]
    track: String,
    #[description = "The primary mood"]
    #[autocomplete = "autocomplete_mood"]
    mood: String,
    #[description = "The intensity"]
    #[autocomplete = "autocomplete_intensity"]
    intensity: String,
    #[description = "Optional scene function"]
    #[autocomplete = "autocomplete_function"]
    function_tag: Option<String>,
) -> Result<(), Error> {
    let db_pool = &ctx.data().db_pool;
    let info = require_track(db_pool, &VideoId::from(track)).await?;
    require_value(MOODS, &mood, "mood")?;
    require_value(INTENSITIES, &intensity, "intensity")?;
    if let Some(value) = &function_tag {
        require_value(FUNCTIONS, value, "function")?;
    }
    set_track_taxonomy(
        db_pool,
        &info.id,
        &mood,
        &intensity,
        function_tag.as_deref(),
    )
    .await?;

    ctx.say(format!("Set taxonomy for track `{}`", info.title))
        .await?;
    Ok(())
}

/// Add a controlled texture to a track
#[poise::command(slash_command)]
pub async fn add_texture(
    ctx: PoiseContext<'_>,
    #[description = "The track to update"]
    #[autocomplete = "autocomplete_track"]
    track: String,
    #[description = "The texture to add"]
    #[autocomplete = "autocomplete_texture"]
    texture: String,
) -> Result<(), Error> {
    require_value(TEXTURES, &texture, "texture")?;
    let db_pool = &ctx.data().db_pool;
    let info = require_track(db_pool, &VideoId::from(track)).await?;
    insert_track_texture(db_pool, &info.id, &texture).await?;
    ctx.say(format!(
        "Added texture `{texture}` to track `{}`",
        info.title
    ))
    .await?;
    Ok(())
}

/// Add a controlled environment to a track
#[poise::command(slash_command)]
pub async fn add_environment(
    ctx: PoiseContext<'_>,
    #[description = "The track to update"]
    #[autocomplete = "autocomplete_track"]
    track: String,
    #[description = "The environment to add"]
    #[autocomplete = "autocomplete_environment"]
    environment: String,
) -> Result<(), Error> {
    require_value(ENVIRONMENTS, &environment, "environment")?;
    let db_pool = &ctx.data().db_pool;
    let info = require_track(db_pool, &VideoId::from(track)).await?;
    insert_track_environment(db_pool, &info.id, &environment).await?;
    ctx.say(format!(
        "Added environment `{environment}` to track `{}`",
        info.title
    ))
    .await?;
    Ok(())
}

/// Add a free-form label which is excluded from automatic playlist selection
#[poise::command(slash_command)]
pub async fn add_label(
    ctx: PoiseContext<'_>,
    #[description = "The track to update"]
    #[autocomplete = "autocomplete_track"]
    track: String,
    #[description = "The custom label to add"] label: String,
) -> Result<(), Error> {
    let db_pool = &ctx.data().db_pool;
    let info = require_track(db_pool, &VideoId::from(track)).await?;
    insert_track_label(db_pool, &info.id, &label).await?;
    ctx.say(format!("Added label `{label}` to track `{}`", info.title))
        .await?;
    Ok(())
}

/// Set a track's title, artist, or origin
#[poise::command(
    slash_command,
    subcommands("title", "artist", "origin"),
    subcommand_required
)]
#[allow(clippy::unused_async)]
pub async fn set_metadata(_ctx: PoiseContext<'_>) -> Result<(), Error> {
    Ok(())
}

/// Set a track's title
#[poise::command(slash_command)]
pub async fn title(
    ctx: PoiseContext<'_>,
    #[description = "The track to adjust"]
    #[autocomplete = "autocomplete_track"]
    track: String,
    #[description = "The new title to give the track"] new_title: String,
) -> Result<(), Error> {
    let db_pool = &ctx.data().db_pool;
    let track_id = VideoId::from(track);
    let info = require_track(db_pool, &track_id).await?;

    update_track_title(db_pool, &info.id, &new_title).await?;

    ctx.say(format!(
        "Set new title `{}` for track `{}`",
        new_title, info.title
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
        new_artist, info.title
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
        new_origin, info.title
    ))
    .await?;
    Ok(())
}

/// Fix missing metadata for an incomplete track
#[poise::command(slash_command)]
pub async fn fix(
    ctx: PoiseContext<'_>,
    #[description = "The incomplete track to fix"]
    #[autocomplete = "autocomplete_incomplete_track"]
    track: String,
    #[description = "New title for the track"] new_title: Option<String>,
    #[description = "New artist for the track"]
    #[autocomplete = "autocomplete_artist"]
    new_artist: Option<String>,
    #[description = "New origin for the track"]
    #[autocomplete = "autocomplete_origin"]
    new_origin: Option<String>,
) -> Result<(), Error> {
    if new_title.is_none() && new_artist.is_none() && new_origin.is_none() {
        ctx.say("Please provide at least one field to update.")
            .await?;
        return Ok(());
    }

    let db_pool = &ctx.data().db_pool;
    let track_id = VideoId::from(track);
    let info = require_track(db_pool, &track_id).await?;

    let mut updated_fields: Vec<String> = Vec::new();

    if let Some(ref title) = new_title {
        update_track_title(db_pool, &info.id, title).await?;
        updated_fields.push(format!("title → `{title}`"));
    }

    if let Some(ref artist) = new_artist {
        let artist_id = get_or_insert_metadata_id(db_pool, MetadataKind::Artist, artist).await?;
        update_track_artist(db_pool, &info.id, artist_id).await?;
        updated_fields.push(format!("artist → `{artist}`"));
    }

    if let Some(ref origin) = new_origin {
        let origin_id = get_or_insert_metadata_id(db_pool, MetadataKind::Origin, origin).await?;
        update_track_origin(db_pool, &info.id, origin_id).await?;
        updated_fields.push(format!("origin → `{origin}`"));
    }

    ctx.say(format!(
        "Updated `{}`: {}",
        info.title,
        updated_fields.join(", ")
    ))
    .await?;

    Ok(())
}
