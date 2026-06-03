use crate::definitions::{Context, Error, VideoId};
use crate::autocomplete::{autocomplete_track, autocomplete_tag, autocomplete_origin, autocomplete_artist};
use crate::library::{get_id_or_insert, get_youtube_id, process_ytdlp_json, require_track};
use std::process::Command;
use serde_json::Value;

pub async fn download_direct(
    ctx: Context<'_>,
    yt_link: String,
    track_artist: Option<String>,
    track_origin: Option<String>,
    track_title: Option<String>,
) -> Result<(String, String, String), Error> {
    let video_id = get_youtube_id(&yt_link).ok_or("Invalid YouTube link")?;

    let db_pool = &ctx.data().db_pool;

    // guard against duplicate downloads
    match sqlx::query_scalar::<_, String>(
        "SELECT track_title FROM tracks WHERE id = ?1",
    )
    .bind(&video_id)
    .fetch_optional(db_pool)
    .await?
    {
        Some(title) => {
            ctx.say(format!(
                "This track exists in the database already as `{}`.",
                title
            ))
            .await?;

            let (title_db, artist_db): (String, String) = sqlx::query_as(
                "SELECT track_title, artist_id FROM tracks WHERE id = ?1",
            )
            .bind(&video_id)
            .fetch_one(db_pool)
            .await?;

            let artist_name: String = artist_db;

            return Ok((video_id, title_db, artist_name));
        }
        None => {}
    }

    ctx.defer().await?;

    // Download the track using yt-dlp
    let output = Command::new("./yt-dlp")
        .arg("-t")
        .arg("mp3")
        .arg("-o")
        .arg("audio/%(id)s.%(ext)s")
        .arg("--no-playlist")
        .arg("--write-info-json")
        .arg("--no-progress")
        .arg("--cookies")
        .arg("cookies.txt")
        .arg(&yt_link)
        .output()
        .expect("Failed to execute yt-dlp");

    if !output.status.success() {
        return Err(format!(
            "yt-dlp failed with error: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }

    let slim = process_ytdlp_json(video_id.clone()).map_err(|e| {
        format!(
            "Failed to process metadata JSON for video ID `{}`: {}",
            video_id, e
        )
    })?;

    let track_title = track_title.unwrap_or_else(|| {
        slim.get("title")
            .and_then(Value::as_str)
            .unwrap_or("Unknown Title")
            .to_string()
    });

    let track_artist = track_artist.unwrap_or_else(|| {
        "No artist provided".to_string()
    });

    let track_origin = track_origin.unwrap_or_else(|| {
        "No origin provided".to_string()
    });

    let artist_id = get_id_or_insert(db_pool, "artist", &track_artist).await?;
    let origin_id = get_id_or_insert(db_pool, "origin", &track_origin).await?;

    sqlx::query(
        "INSERT INTO tracks (id, upload_date, yt_title, track_title, artist_id, origin_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )
    .bind(&video_id)
    .bind(
        slim.get("upload_date")
            .and_then(Value::as_str)
            .unwrap_or("Unknown Date"),
    )
    .bind(
        slim.get("title")
            .and_then(Value::as_str)
            .unwrap_or("Unknown Title"),
    )
    .bind(&track_title)
    .bind(artist_id)
    .bind(origin_id)
    .execute(db_pool)
    .await?;

    ctx.say(format!(
        "File downloaded and added to the library: `{}`",
        track_title
    ))
    .await?;

    Ok((video_id, track_title, track_artist))
}

/// Download a track from a YouTube link
#[poise::command(slash_command)]
pub async fn download(
    ctx: Context<'_>,
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
    ctx: Context<'_>,
    #[description = "The track to reset the tags of"]
    #[autocomplete = "autocomplete_track"]
    track: String,
) -> Result<(), Error> {
    let db_pool = &ctx.data().db_pool;

    let info = require_track(db_pool, &VideoId::from(track)).await?;

    sqlx::query("DELETE FROM track_tags WHERE track_id = ?1")
        .bind(info.id.as_str())
        .execute(db_pool)
        .await?;

    ctx.say(format!("Reset tags for track `{}`", info.title))
        .await?;

    Ok(())
}

/// Add a new arbitrary tag to a track
#[poise::command(slash_command)]
pub async fn add_tag(
    ctx: Context<'_>,
    #[description = "The track to add a tag to"]
    #[autocomplete = "autocomplete_track"]
    track: String,
    #[description = "The tag to add"]
    #[autocomplete = "autocomplete_tag"]
    tag: String,
) -> Result<(), Error> {
    let db_pool = &ctx.data().db_pool;

    let info = require_track(db_pool, &VideoId::from(track)).await?;

    let tag_id = get_id_or_insert(db_pool, "tag", &tag).await?;

    sqlx::query("INSERT OR IGNORE INTO track_tags (track_id, tag_id) VALUES (?1, ?2)")
        .bind(info.id.as_str())
        .bind(tag_id)
        .execute(db_pool)
        .await?;

    ctx.say(format!(
        "Tag `{}` added to track `{}`",
        tag,
        info.title
    ))
    .await?;

    Ok(())
}

/// Set a track's title, artist, or origin
#[poise::command(slash_command, subcommands("title", "artist", "origin"), subcommand_required)]
pub async fn set_metadata(
    _ctx: Context<'_>,
) -> Result<(), Error> {
    Ok(())
}

/// Set a track's title
#[poise::command(slash_command)]
pub async fn title(
    ctx: Context<'_>,
    #[description = "The track to adjust"]
    #[autocomplete = "autocomplete_track"]
    track: String,
    #[description = "The new title to give the track"]
    new_title: String,
) -> Result<(), Error> {
    let db_pool = &ctx.data().db_pool;

    let track_id = VideoId::from(track);
    let info = require_track(db_pool, &track_id).await?;

    let old_title = info.title.clone();

    sqlx::query("UPDATE tracks SET track_title = ?1 WHERE id = ?2")
        .bind(&new_title)
        .bind(info.id.as_str())
        .execute(db_pool)
        .await?;

    ctx.say(format!(
        "Set new title `{}` for track `{}`",
        new_title,
        old_title
    ))
    .await?;

    Ok(())
}


/// Set a track's artist
#[poise::command(slash_command)]
pub async fn artist(
    ctx: Context<'_>,
    #[description = "The track to adjust"]
    #[autocomplete = "autocomplete_track"]
    track: String,
    #[description = "The new artist for the track"]
    #[autocomplete = "autocomplete_artist"]
    new_artist: String,
) -> Result<(), Error> {
    let db_pool = &ctx.data().db_pool;

    let info = require_track(db_pool, &VideoId::from(track)).await?;

    let artist_id = get_id_or_insert(db_pool, "artist", &new_artist).await?;

    sqlx::query("UPDATE tracks SET artist_id = ?1 WHERE id = ?2")
        .bind(artist_id)
        .bind(info.id.as_str())
        .execute(db_pool)
        .await?;

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
    ctx: Context<'_>,
    #[description = "The track to adjust"]
    #[autocomplete = "autocomplete_track"]
    track: String,
    #[description = "The new origin for the track"]
    #[autocomplete = "autocomplete_origin"]
    new_origin: String,
) -> Result<(), Error> {
    let db_pool = &ctx.data().db_pool;

    let info = require_track(db_pool, &VideoId::from(track)).await?;

    let origin_id = get_id_or_insert(db_pool, "origin", &new_origin).await?;

    sqlx::query("UPDATE tracks SET origin_id = ?1 WHERE id = ?2")
        .bind(origin_id)
        .bind(info.id.as_str())
        .execute(db_pool)
        .await?;

    ctx.say(format!(
        "Set new origin `{}` for track `{}`",
        new_origin,
        info.title
    ))
    .await?;

    Ok(())
}