use crate::definitions::{Context, Error};
use crate::autocomplete::{autocomplete_track, autocomplete_tag, autocomplete_origin, autocomplete_artist};
use crate::library::{get_id_or_insert, get_youtube_id, process_ytdlp_json};
use std::process::Command;
use serde_json::Value;

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
    #[description = "The actual title of the track"] track_title: Option<String>,
) -> Result<(), Error> {

    let video_id = get_youtube_id(&yt_link).ok_or("Invalid YouTube link")?;

    // guard against duplicate downloads
    let db_pool = &ctx.data().db_pool;
    match sqlx::query_scalar::<_, String>("SELECT track_title FROM tracks WHERE id = ?1")
        .bind(&video_id)
        .fetch_optional(db_pool)
        .await
        .unwrap()
    {
        Some(title) => {
            ctx.say(format!("This track exists in the database already as `{}`.", title)).await?;
            return Ok(())
        }
        None => ()
    }

    ctx.defer().await?;

    // Download the track using yt-dlp
    let output = Command::new("yt-dlp")
        .arg("-t")
        .arg("mp3")
        .arg("-o")
        .arg("audio/%(id)s.%(ext)s")
        .arg("--no-playlist")
        .arg("--write-info-json")
        .arg("--no-progress")
        .arg(yt_link)
        .output()
        .expect("Failed to execute yt-dlp");

    if !output.status.success() {
        return Err(format!(
            "yt-dlp failed with error: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }

    // Process the downloaded metadata JSON
    let slim = process_ytdlp_json(video_id.clone()).map_err(|e| {
        format!(
            "Failed to process metadata JSON for video ID `{}`: {}",
            video_id, e
        )
    })?;

    // Extract metadata or use provided values
    let track_title = track_title.unwrap_or_else(|| {
        slim.get("title")
            .and_then(Value::as_str)
            .unwrap_or("Unknown Title")
            .to_string()
    });

    let track_artist = track_artist.unwrap_or_else(|| "No artist provided".to_string());

    let track_origin = track_origin.unwrap_or_else(|| "No origin provided".to_string());

    // Insert the track metadata into the database
    let db_pool = &ctx.data().db_pool;
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
    .bind(get_id_or_insert(db_pool, "artist", &track_artist).await)
    .bind(get_id_or_insert(db_pool, "origin", &track_origin).await)
    .execute(db_pool)
    .await?;

    ctx.say(format!("File downloaded and added to the library: `{}`", track_title))
        .await?;
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

    // Check if the track exists in the database
    let track_exists: Option<String> = sqlx::query_scalar("SELECT id FROM tracks WHERE id = ?1")
        .bind(&track)
        .fetch_optional(db_pool)
        .await?;

    if let Some(track_id) = track_exists {
        // Delete all tag associations for the track from the `track_tags` table
        sqlx::query("DELETE FROM track_tags WHERE track_id = ?1")
            .bind(&track_id)
            .execute(db_pool)
            .await?;

        let track_title: String = sqlx::query_scalar("SELECT track_title FROM tracks WHERE id = ?1")
            .bind(&track)
            .fetch_optional(db_pool)
            .await?
            .unwrap();

        ctx.say(format!("Reset tags for track `{}`", track_title))
            .await?;
    } else {
        ctx.say(format!("The track `{}` could not be found in the database.", track)).await?;
    }

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

    // Check if the track exists in the database
    let track_exists: Option<String> = sqlx::query_scalar("SELECT id FROM tracks WHERE id = ?1")
        .bind(&track)
        .fetch_optional(db_pool)
        .await?;

    if let Some(track_id) = track_exists {
        let tag_id = get_id_or_insert(db_pool, "tag", &tag).await;

        // Insert the association into the `track_tags` table
        sqlx::query("INSERT OR IGNORE INTO track_tags (track_id, tag_id) VALUES (?1, ?2)")
            .bind(&track_id)
            .bind(tag_id)
            .execute(db_pool)
            .await?;

        let track_title: String = sqlx::query_scalar("SELECT track_title FROM tracks WHERE id = ?1")
            .bind(&track)
            .fetch_optional(db_pool)
            .await?
            .unwrap();

        ctx.say(format!("Tag `{}` added to track `{}`", tag, track_title)).await?;
    } else {
        ctx.say(format!("The track `{}` could not be found in the database.", track)).await?;
    }

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

    // Check if the track exists in the database
    let track_exists: Option<String> = sqlx::query_scalar("SELECT id FROM tracks WHERE id = ?1")
        .bind(&track)
        .fetch_optional(db_pool)
        .await?;

    if let Some(track_id) = track_exists {
        // Update the track's title in the database
        let old_title: String = sqlx::query_scalar("SELECT track_title FROM tracks WHERE id = ?1")
            .bind(&track)
            .fetch_optional(db_pool)
            .await?
            .unwrap();

        sqlx::query("UPDATE tracks SET track_title = ?1 WHERE id = ?2")
            .bind(&new_title)
            .bind(&track_id)
            .execute(db_pool)
            .await?;

        ctx.say(format!("Set new title `{}` for track `{}`", new_title, old_title))
        .await?;
    } else {
        ctx.say(format!("The track `{}` could not be found in the database.", track)).await?;
    }

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

    // Check if the track exists in the database
    let track_exists: Option<String> = sqlx::query_scalar("SELECT id FROM tracks WHERE id = ?1")
        .bind(&track)
        .fetch_optional(db_pool)
        .await?;

    if let Some(track_id) = track_exists {
        // Update the track's artist in the database
        sqlx::query("UPDATE tracks SET artist_id = ?1 WHERE id = ?2")
            .bind(get_id_or_insert(db_pool, "artist", &new_artist).await)
            .bind(&track_id)
            .execute(db_pool)
            .await?;

        let track_title: String = sqlx::query_scalar("SELECT track_title FROM tracks WHERE id = ?1")
            .bind(&track)
            .fetch_optional(db_pool)
            .await?
            .unwrap();

        ctx.say(format!("Set new artist `{}` for track `{}`",new_artist, track_title)).await?;
    } else {
        ctx.say(format!("The track `{}` could not be found in the database.", track)).await?;
    }

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

    // Check if the track exists in the database
    let track_exists: Option<String> = sqlx::query_scalar("SELECT id FROM tracks WHERE id = ?1")
        .bind(&track)
        .fetch_optional(db_pool)
        .await?;

    if let Some(track_id) = track_exists {
        // Update the track's origin in the database
        sqlx::query("UPDATE tracks SET origin_id = ?1 WHERE id = ?2")
            .bind(get_id_or_insert(db_pool, "origin", &new_origin).await)
            .bind(&track_id)
            .execute(db_pool)
            .await?;

        let track_title: String = sqlx::query_scalar("SELECT track_title FROM tracks WHERE id = ?1")
            .bind(&track)
            .fetch_optional(db_pool)
            .await?
            .unwrap();

        ctx.say(format!("Set new origin `{}` for track `{}`", new_origin, track_title))
        .await?;
    } else {
        ctx.say(format!("The track `{}` could not be found in the database.", track)).await?;
    }

    Ok(())
}