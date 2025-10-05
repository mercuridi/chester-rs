////////////////////////////////////////////////////////////////////////////////
// Imports
use std::process::Command;

use serde_json::Value;
use songbird::input::File as SongbirdFile;
use songbird::input::cached::Compressed;
use songbird::driver::Bitrate;
use songbird::tracks::LoopState;
use crate::definitions::{Context, Error, Data};
use crate::json_handling::process_ytdlp_json;
use crate::library::{get_id_or_insert, get_vc_id, get_youtube_id, join_vc, fmt_library_col};
use crate::autocomplete::*;
use crate::constants::*;

////////////////////////////////////////////////////////////////////////////////
// Command definitions

// Commands: Bot management
/// Force-register commands - only invokes with ">"
#[poise::command(prefix_command)]
pub async fn register(ctx: Context<'_>) -> Result<(), Error> {
    poise::builtins::register_application_commands_buttons(ctx).await?;
    Ok(())
}

/// Shows help for commands
#[poise::command(prefix_command, slash_command)]
pub async fn help(
    ctx: Context<'_>,
    #[description = "Specific command to show help about"]
    #[autocomplete = "poise::builtins::autocomplete_command"]
    command: Option<String>,
) -> Result<(), Error> {
    poise::builtins::help(
        ctx,
        command.as_deref(),
        poise::builtins::HelpConfiguration {
            extra_text_at_bottom: "Chester is a Discord music bot that won't ask for your money.",
            ..Default::default()
        },
    )
    .await?;
    Ok(())
}

// Commands: Library management

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
    ctx.defer().await?;

    let video_id = get_youtube_id(&yt_link).ok_or("Invalid YouTube link")?;

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

// Commands: Music control

/// Joins your voice channel
#[poise::command(slash_command)]
pub async fn join(
    ctx: Context<'_>,
) -> Result<(), Error> {
    let guild = ctx.guild().expect("Must be in a guild to use voice").clone();
    let vc_id = get_vc_id(ctx).await?;

    join_vc(ctx, guild, vc_id).await?;

    ctx.say("Joined your voice channel! 🎶").await?;
    Ok(())
}

/// Plays a selected track from the library
#[poise::command(slash_command)]
pub async fn play(
    ctx: Context<'_>,
    #[description = "Track to play now"]
    #[autocomplete = "autocomplete_track"]
    track: String,
) -> Result<(), Error> {
    let db_pool = &ctx.data().db_pool;

    // Check if the track exists in the database
    let track_metadata: Option<(String, String)> = sqlx::query_as(
        "SELECT track_title, artists.artist FROM tracks 
        LEFT JOIN artists ON tracks.artist_id = artists.id
        WHERE tracks.id = ?1",
    )
    .bind(&track)
    .fetch_optional(db_pool)
    .await?;

    if let Some((track_title, track_artist)) = track_metadata {
        let guild = ctx.guild().expect("Must be in a guild to use voice").clone();
        let vc_id = get_vc_id(ctx).await?;

        let serenity_ctx = ctx.serenity_context();

        let manager = songbird::get(serenity_ctx)
            .await
            .expect("Songbird was not initialized")
            .clone();

        join_vc(ctx, guild.clone(), vc_id).await?;
        let track_path = format!("audio/{track}.mp3");
        println!("{}", track_path.clone());

        let path = std::env::current_dir()?;
        println!("The current directory is {}", path.display());

        let song_src = Compressed::new(
            SongbirdFile::new(track_path).into(),
            Bitrate::BitsPerSecond(128_000),
        )
        .await
        .expect("An error occurred constructing the track source");
        let _ = song_src.raw.spawn_loader();

        let data: &Data = ctx.data();

        if let Some(handler_lock) = manager.get(guild.id.clone()) {
            let mut handler = handler_lock.lock().await;
            let track_handle = handler.play_only_input(song_src.into());
            let _ = track_handle.enable_loop()?;
            let mut handles = data.track_handles.write().await; // tokio::sync::RwLock
            handles.insert(guild.id, track_handle);
        }

        ctx.say(format!(
            "Now playing: `{}` by `{}`",
            track_title, track_artist
        ))
        .await?;
    } else {
        ctx.say(format!("The track `{}` could not be found in the database.", track)).await?;
    }

    Ok(())
}

/// Loop or un‐loop the currently playing track.
#[poise::command(slash_command, prefix_command)]
pub async fn loop_track(
    ctx: Context<'_>,
) -> Result<(), Error> {
    // Make sure we're in a guild
    let guild_id = if let Some(g) = ctx.guild_id() {
        g
    } else {
        return Err(format!("Looping only works in a server").into())
    };

    // See if there's a current track
    let data: &Data = ctx.data();
    let handles = data.track_handles.read().await; // tokio::sync::RwLock
    if let Some(track_handle) = handles.get(&guild_id) {
        let handle_info = track_handle.clone().get_info().await?;
        let loops = handle_info.loops;
        let new_state: bool;
        match loops {
            LoopState::Infinite => {
                let _ = track_handle.disable_loop()?;
                new_state = false;
            },
            LoopState::Finite(_) => {
                let _ = track_handle.enable_loop()?;
                new_state = true;
            }
        }
        ctx.say(format!("Looping {}", if new_state { "enabled" } else { "disabled" })).await?;
    } else {
        ctx.say("No track is currently playing.").await?;
    };
    Ok(())
}

/// Leaves a joined voice channel
#[poise::command(slash_command)]
pub async fn leave(
    ctx: Context<'_>,
) -> Result<(), Error> {

    let guild = ctx.guild().expect("Must be in a guild to use voice").clone();
    let _vc_id = get_vc_id(ctx).await?;

    let serenity_ctx = ctx.serenity_context();

    let manager = songbird::get(serenity_ctx)
        .await
        .expect("Songbird was not initialized")
        .clone();

    manager.remove(guild.id).await?;

    ctx.say("Left the voice channel").await?;

    Ok(())
}

/// /library
#[poise::command(slash_command)]
pub async fn library(ctx: Context<'_>) -> Result<(), Error> {
    // Pass a default sort order, e.g., by track title
    library_sorted(ctx, "tracks.track_title").await
}

/// /library title
#[poise::command(slash_command)]
pub async fn library_title(ctx: Context<'_>) -> Result<(), Error> {
    // SQL query to fetch only track titles, sorted by title
    let query = "
        SELECT track_title
        FROM tracks
        ORDER BY track_title
    ";

    let db_pool = &ctx.data().db_pool;

    let titles: Vec<String> = sqlx::query_as::<_, (String,)>(query)
        .fetch_all(db_pool)
        .await
        .unwrap_or_else(|err| {
            println!("Database query failed: {}", err);
            Vec::new()
        })
        .into_iter()
        .map(|(title,)| {
            // Fill the column fully
            fmt_library_col(title, LIB_TIT_COLUMN_WIDTH_TITLE)
        })
        .collect();

    // Header
    let header = fmt_library_col("TITLE".to_string(), LIB_TIT_COLUMN_WIDTH_TITLE);
    let separator = ROW_SEPARATOR.repeat(LIBRARY_ROW_MAX_WIDTH);

    // Paginate
    let mut pages: Vec<String> = Vec::new();
    for chunk in titles.chunks(MAX_RESULTS_PER_PAGE) {
        let rows = chunk.join("\n");
        let body = format!("{}\n{}\n{}", header, separator, rows);
        pages.push(format!("```text\n{}\n```", body));
    }

    let page_refs: Vec<&str> = pages.iter().map(|s| s.as_str()).collect();
    poise::samples::paginate(ctx, &page_refs).await?;

    Ok(())
}

/// /library artist
#[poise::command(slash_command)]
pub async fn library_artist(ctx: Context<'_>) -> Result<(), Error> {
    // SQL query to fetch artist and title
    let query = "
        SELECT artists.artist, tracks.track_title
        FROM tracks
        LEFT JOIN artists ON tracks.artist_id = artists.id
        ORDER BY artists.artist, tracks.track_title
    ";

    let db_pool = &ctx.data().db_pool;

    let entries: Vec<String> = sqlx::query_as::<_, (String, String)>(query)
        .fetch_all(db_pool)
        .await
        .unwrap_or_else(|err| {
            println!("Database query failed: {}", err);
            Vec::new()
        })
        .into_iter()
        .map(|(artist, title)| {
            format!(
                "{}{}{}",
                fmt_library_col(artist, LIB_ART_COLUMN_WIDTH_ARTIST),
                LIBRARY_SEPARATOR,
                fmt_library_col(title, LIB_ART_COLUMN_WIDTH_TITLE),
            )
        })
        .collect();

    // Header
    let header = format!(
        "{}{}{}",
        fmt_library_col("ARTIST".to_string(), LIB_ART_COLUMN_WIDTH_ARTIST),
        LIBRARY_SEPARATOR,
        fmt_library_col("TITLE".to_string(), LIB_ART_COLUMN_WIDTH_TITLE),
    );
    let separator = ROW_SEPARATOR.repeat(LIBRARY_ROW_MAX_WIDTH);

    // Paginate
    let mut pages: Vec<String> = Vec::new();
    for chunk in entries.chunks(MAX_RESULTS_PER_PAGE) {
        let rows = chunk.join("\n");
        let body = format!("{}\n{}\n{}", header, separator, rows);
        pages.push(format!("```text\n{}\n```", body));
    }

    let page_refs: Vec<&str> = pages.iter().map(|s| s.as_str()).collect();
    poise::samples::paginate(ctx, &page_refs).await?;

    Ok(())
}


/// /library origin
#[poise::command(slash_command)]
pub async fn library_origin(ctx: Context<'_>) -> Result<(), Error> {
    // SQL query to fetch origin and title
    let query = "
        SELECT origins.origin, tracks.track_title
        FROM tracks
        LEFT JOIN origins ON tracks.origin_id = origins.id
        ORDER BY origins.origin, tracks.track_title
    ";

    let db_pool = &ctx.data().db_pool;

    let entries: Vec<String> = sqlx::query_as::<_, (String, String)>(query)
        .fetch_all(db_pool)
        .await
        .unwrap_or_else(|err| {
            println!("Database query failed: {}", err);
            Vec::new()
        })
        .into_iter()
        .map(|(origin, title)| {
            format!(
                "{}{}{}",
                fmt_library_col(origin, LIB_ORI_COLUMN_WIDTH_ORIGIN),
                LIBRARY_SEPARATOR,
                fmt_library_col(title, LIB_ORI_COLUMN_WIDTH_TITLE),
            )
        })
        .collect();

    // Header
    let header = format!(
        "{}{}{}",
        fmt_library_col("ORIGIN".to_string(), LIB_ORI_COLUMN_WIDTH_ORIGIN),
        LIBRARY_SEPARATOR,
        fmt_library_col("TITLE".to_string(), LIB_ORI_COLUMN_WIDTH_TITLE),
    );
    let separator = ROW_SEPARATOR.repeat(LIBRARY_ROW_MAX_WIDTH);

    // Paginate
    let mut pages: Vec<String> = Vec::new();
    for chunk in entries.chunks(MAX_RESULTS_PER_PAGE) {
        let rows = chunk.join("\n");
        let body = format!("{}\n{}\n{}", header, separator, rows);
        pages.push(format!("```text\n{}\n```", body));
    }

    let page_refs: Vec<&str> = pages.iter().map(|s| s.as_str()).collect();
    poise::samples::paginate(ctx, &page_refs).await?;

    Ok(())
}


/// /library tags
#[poise::command(slash_command)]
pub async fn library_tags(ctx: Context<'_>) -> Result<(), Error> {
    // SQL query to fetch tag and track title pairs
    let query = "
        SELECT tags.tag, tracks.track_title
        FROM tracks
        LEFT JOIN track_tags ON tracks.id = track_tags.track_id
        LEFT JOIN tags ON track_tags.tag_id = tags.id
        ORDER BY 
            CASE WHEN tags.tag IS NULL THEN 1 ELSE 0 END, 
            tags.tag, 
            tracks.track_title
    ";

    let db_pool = &ctx.data().db_pool;

    let entries: Vec<String> = sqlx::query_as::<_, (Option<String>, String)>(query)
        .fetch_all(db_pool)
        .await
        .unwrap_or_else(|err| {
            println!("Database query failed: {}", err);
            Vec::new()
        })
        .into_iter()
        .map(|(tag_opt, title)| {
            let tag = tag_opt.unwrap_or_else(|| "No tag".to_string());
            format!(
                "{}{}{}",
                fmt_library_col(tag, LIB_TAG_COLUMN_WIDTH_TAGS),
                LIBRARY_SEPARATOR,
                fmt_library_col(title, LIB_TAG_COLUMN_WIDTH_TITLE),
            )
        })
        .collect();

    // Header
    let header = format!(
        "{}{}{}",
        fmt_library_col("TAG".to_string(), LIB_TAG_COLUMN_WIDTH_TAGS),
        LIBRARY_SEPARATOR,
        fmt_library_col("TITLE".to_string(), LIB_TAG_COLUMN_WIDTH_TITLE),
    );
    let separator = ROW_SEPARATOR.repeat(LIBRARY_ROW_MAX_WIDTH);

    // Paginate
    let mut pages: Vec<String> = Vec::new();
    for chunk in entries.chunks(MAX_RESULTS_PER_PAGE) {
        let rows = chunk.join("\n");
        let body = format!("{}\n{}\n{}", header, separator, rows);
        pages.push(format!("```text\n{}\n```", body));
    }

    let page_refs: Vec<&str> = pages.iter().map(|s| s.as_str()).collect();
    poise::samples::paginate(ctx, &page_refs).await?;

    Ok(())
}



/// Return a paginated printout of the entire library
pub async fn library_sorted(ctx: Context<'_>, sort: &str) -> Result<(), Error> {
    let query = format!(
        "
        SELECT DISTINCT tracks.track_title, artists.artist, origins.origin, GROUP_CONCAT(tags.tag, ', ') AS tags
        FROM tracks
        LEFT JOIN track_tags ON tracks.id = track_tags.track_id
        LEFT JOIN tags ON track_tags.tag_id = tags.id
        LEFT JOIN artists ON tracks.artist_id = artists.id
        LEFT JOIN origins ON tracks.origin_id = origins.id
        GROUP BY tracks.id, tracks.track_title, artists.artist, origins.origin
        ORDER BY {}
        ",
        sort
    );

    let db_pool = &ctx.data().db_pool;

    let library: Vec<String> = sqlx::query_as(&query)
        .fetch_all(db_pool)
        .await
        .unwrap_or_else(|err| {
            println!("Database query failed: {}", err);
            Vec::new()
        })
        .into_iter()
        .map(|(title, artist, origin, tags): (String, String, String, Option<String>)| {
            let tags_display = tags.unwrap_or_else(|| "No tags".to_string());
        
            format!(
                "{}{}{}{}{}{}{}",
                fmt_library_col(title, LIBRARY_COLUMN_WIDTH_TITLE),
                LIBRARY_SEPARATOR,
                fmt_library_col(artist,LIBRARY_COLUMN_WIDTH_ARTIST ),
                LIBRARY_SEPARATOR,
                fmt_library_col(origin, LIBRARY_COLUMN_WIDTH_ORIGIN),
                LIBRARY_SEPARATOR,
                fmt_library_col(tags_display, LIBRARY_COLUMN_WIDTH_TAGS),
            )
        })
        .collect();

    let mut pages: Vec<String> = Vec::new();

    // Build the header once
    let header = format!(
        "{}{}{}{}{}{}{}",
        fmt_library_col("TITLE".to_string(), LIBRARY_COLUMN_WIDTH_TITLE),
        LIBRARY_SEPARATOR,
        fmt_library_col("ARTIST".to_string(), LIBRARY_COLUMN_WIDTH_ARTIST),
        LIBRARY_SEPARATOR,
        fmt_library_col("ORIGIN".to_string(), LIBRARY_COLUMN_WIDTH_ORIGIN),
        LIBRARY_SEPARATOR,
        fmt_library_col("TAGS".to_string(), LIBRARY_COLUMN_WIDTH_TAGS),
    );

    // Separator (56 chars wide: fill with '-')
    let separator = "-".repeat(LIBRARY_ROW_MAX_WIDTH);

    for chunk in library.chunks(MAX_RESULTS_PER_PAGE) {
        // Format the rows
        let rows = chunk.join("\n");

        // Put together: header + separator + rows
        let body = format!("{}\n{}\n{}", header, separator, rows);

        // Wrap in code block
        let formatted = format!("```text\n{}\n```", body);
        pages.push(formatted);
    }

    let page_refs: Vec<&str> = pages.iter().map(|s| s.as_str()).collect();
    poise::samples::paginate(ctx, &page_refs).await?;


    Ok(())
}

/// Toggles pause/unpause for the currently playing track
#[poise::command(slash_command)]
pub async fn pause(
    ctx: Context<'_>,
) -> Result<(), Error> {
    // Ensure the command is used in a guild
    let guild_id = if let Some(g) = ctx.guild_id() {
        g
    } else {
        return Err("Pause command can only be used in a server.".into());
    };

    // Access the track handle for the current guild
    let data: &Data = ctx.data();
    let handles = data.track_handles.read().await; // tokio::sync::RwLock
    if let Some(track_handle) = handles.get(&guild_id) {
        let handle_info = track_handle.clone().get_info().await?;
        if handle_info.playing == songbird::tracks::PlayMode::Play {
            track_handle.pause()?;
            ctx.say("Paused the currently playing track.").await?;
        } else {
            track_handle.play()?;
            ctx.say("Resumed the currently paused track.").await?;
        }
    } else {
        ctx.say("No track is currently playing.").await?;
    }

    Ok(())
}