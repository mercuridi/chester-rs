use serde_json::Value;
use sqlx::{SqlitePool, Row};

use crate::definitions::{Error, MetadataKind, TrackInfo, VideoId};

pub async fn get_or_insert_metadata_id(
    db_pool: &SqlitePool,
    kind: MetadataKind,
    value: &str,
) -> Result<i64, Error> {
    let select_sql = kind.select_sql();

    match sqlx::query_scalar::<_, i64>(select_sql)
        .bind(value)
        .fetch_optional(db_pool)
        .await
        .map_err(|e| format!("Database select failed: {}", e))?
    {
        Some(id) => Ok(id),
        None => {
            sqlx::query(kind.insert_sql())
                .bind(value)
                .execute(db_pool)
                .await
                .map_err(|e| format!("Database insert failed: {}", e))?;

            Ok(sqlx::query_scalar::<_, i64>(select_sql)
                .bind(value)
                .fetch_one(db_pool)
                .await
                .map_err(|e| format!("Database fetch after insert failed: {}", e))?)
        }
    }
}

pub async fn insert_new_track(
    db_pool: &SqlitePool,
    video_id: &VideoId,
    slim: &serde_json::Value,
    title: &str,
    artist_id: i64,
    origin_id: i64,
) -> Result<(), Error> {
    sqlx::query(
        "INSERT INTO tracks (
            id,
            upload_date,
            yt_title,
            track_title,
            artist_id,
            origin_id
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )
    .bind(video_id.as_str())
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
    .bind(title)
    .bind(artist_id)
    .bind(origin_id)
    .execute(db_pool)
    .await?;

    Ok(())
}

pub async fn fetch_library_all(db_pool: &SqlitePool) -> Result<Vec<Vec<String>>, Error> {
    let rows = sqlx::query(
        "SELECT tracks.track_title, artists.artist, origins.origin,
                GROUP_CONCAT(tags.tag, ', ') AS tags
         FROM tracks
         LEFT JOIN artists ON tracks.artist_id = artists.id
         LEFT JOIN origins ON tracks.origin_id = origins.id
         LEFT JOIN track_tags ON tracks.id = track_tags.track_id
         LEFT JOIN tags ON track_tags.tag_id = tags.id
         GROUP BY tracks.id
         ORDER BY tracks.track_title",
    )
    .fetch_all(db_pool)
    .await
    .map_err(|e| format!("Database query failed: {}", e))?;

    Ok(rows.into_iter().map(|row| vec![
        row.try_get::<String, _>(0).unwrap_or_else(|_| "No title".to_string()),
        row.try_get::<String, _>(1).unwrap_or_else(|_| "No artist".to_string()),
        row.try_get::<String, _>(2).unwrap_or_else(|_| "No origin".to_string()),
        row.try_get::<String, _>(3).unwrap_or_else(|_| "".to_string()),
    ]).collect())
}

pub async fn fetch_library_by_artist(db_pool: &SqlitePool) -> Result<Vec<Vec<String>>, Error> {
    let rows = sqlx::query(
        "SELECT artists.artist, tracks.track_title
         FROM tracks
         LEFT JOIN artists ON tracks.artist_id = artists.id
         ORDER BY artists.artist",
    )
    .fetch_all(db_pool)
    .await
    .map_err(|e| format!("Database query failed: {}", e))?;

    Ok(rows.into_iter().map(|row| vec![
        row.try_get::<String, _>(0).unwrap_or_else(|_| "No artist".to_string()),
        row.try_get::<String, _>(1).unwrap_or_else(|_| "No title".to_string()),
    ]).collect())
}

pub async fn fetch_library_by_origin(db_pool: &SqlitePool) -> Result<Vec<Vec<String>>, Error> {
    let rows = sqlx::query(
        "SELECT origins.origin, tracks.track_title
         FROM tracks
         LEFT JOIN origins ON tracks.origin_id = origins.id
         ORDER BY origins.origin",
    )
    .fetch_all(db_pool)
    .await
    .map_err(|e| format!("Database query failed: {}", e))?;

    Ok(rows.into_iter().map(|row| vec![
        row.try_get::<String, _>(0).unwrap_or_else(|_| "No origin".to_string()),
        row.try_get::<String, _>(1).unwrap_or_else(|_| "No title".to_string()),
    ]).collect())
}

pub async fn fetch_library_by_tag(db_pool: &SqlitePool) -> Result<Vec<Vec<String>>, Error> {
    let rows = sqlx::query(
        "SELECT COALESCE(tags.tag, 'No tags') AS tag, tracks.track_title
         FROM tracks
         LEFT JOIN track_tags ON tracks.id = track_tags.track_id
         LEFT JOIN tags ON track_tags.tag_id = tags.id
         ORDER BY
             CASE WHEN tags.tag IS NULL THEN 1 ELSE 0 END,
             tag,
             tracks.track_title",
    )
    .fetch_all(db_pool)
    .await
    .map_err(|e| format!("Database query failed: {}", e))?;

    Ok(rows.into_iter().map(|row| vec![
        row.try_get::<String, _>(0).unwrap_or_else(|_| "No tags".to_string()),
        row.try_get::<String, _>(1).unwrap_or_else(|_| "No title".to_string()),
    ]).collect())
}

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
         WHERE tracks.id = ?1",
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

pub async fn require_track(
    db_pool: &SqlitePool,
    id: &VideoId,
) -> Result<TrackInfo, Error> {
    lookup_track(db_pool, id)
        .await?
        .ok_or_else(|| "Track could not be found in the database.".into())
}

pub async fn search_metadata(
    db_pool: &SqlitePool,
    kind: MetadataKind,
    needle: &str,
    limit: i64,
) -> Result<Vec<String>, Error> {
    let query = match kind {
        MetadataKind::Artist => "SELECT DISTINCT artist FROM artists WHERE LOWER(artist) LIKE ?1 LIMIT ?2",
        MetadataKind::Origin => "SELECT DISTINCT origin FROM origins WHERE LOWER(origin) LIKE ?1 LIMIT ?2",
        MetadataKind::Tag    => "SELECT DISTINCT tag FROM tags WHERE LOWER(tag) LIKE ?1 LIMIT ?2",
    };

    sqlx::query_scalar(query)
        .bind(format!("%{}%", needle))
        .bind(limit)
        .fetch_all(db_pool)
        .await
        .map_err(|e| format!("Autocomplete metadata query failed: {}", e).into())
}

pub async fn search_tracks(
    db_pool: &SqlitePool,
    needle: &str,
    limit: i64,
) -> Result<Vec<(String, String, String, String, Option<String>)>, Error> {
    sqlx::query_as(
        "SELECT DISTINCT tracks.id, tracks.track_title, artists.artist, origins.origin,
                GROUP_CONCAT(tags.tag, ', ') AS tags
         FROM tracks
         LEFT JOIN track_tags ON tracks.id = track_tags.track_id
         LEFT JOIN tags ON track_tags.tag_id = tags.id
         LEFT JOIN artists ON tracks.artist_id = artists.id
         LEFT JOIN origins ON tracks.origin_id = origins.id
         WHERE LOWER(tracks.track_title) LIKE ?1
            OR LOWER(artists.artist) LIKE ?1
            OR LOWER(origins.origin) LIKE ?1
            OR LOWER(tags.tag) LIKE ?1
         GROUP BY tracks.id, tracks.track_title, artists.artist, origins.origin
         LIMIT ?2",
    )
    .bind(format!("%{}%", needle))
    .bind(limit)
    .fetch_all(db_pool)
    .await
    .map_err(|e| format!("Autocomplete track query failed: {}", e).into())
}

pub async fn delete_track_tags(
    db_pool: &SqlitePool,
    track_id: &VideoId,
) -> Result<(), Error> {
    sqlx::query("DELETE FROM track_tags WHERE track_id = ?1")
        .bind(track_id.as_str())
        .execute(db_pool)
        .await
        .map_err(|e| format!("Failed to delete tags for track {}: {}", track_id.as_str(), e))?;
    Ok(())
}

pub async fn insert_track_tag(
    db_pool: &SqlitePool,
    track_id: &VideoId,
    tag_id: i64,
) -> Result<(), Error> {
    sqlx::query("INSERT OR IGNORE INTO track_tags (track_id, tag_id) VALUES (?1, ?2)")
        .bind(track_id.as_str())
        .bind(tag_id)
        .execute(db_pool)
        .await
        .map_err(|e| format!("Failed to insert tag for track {}: {}", track_id.as_str(), e))?;
    Ok(())
}

pub async fn update_track_title(
    db_pool: &SqlitePool,
    track_id: &VideoId,
    new_title: &str,
) -> Result<(), Error> {
    sqlx::query("UPDATE tracks SET track_title = ?1 WHERE id = ?2")
        .bind(new_title)
        .bind(track_id.as_str())
        .execute(db_pool)
        .await
        .map_err(|e| format!("Failed to update title for track {}: {}", track_id.as_str(), e))?;
    Ok(())
}

pub async fn update_track_artist(
    db_pool: &SqlitePool,
    track_id: &VideoId,
    artist_id: i64,
) -> Result<(), Error> {
    sqlx::query("UPDATE tracks SET artist_id = ?1 WHERE id = ?2")
        .bind(artist_id)
        .bind(track_id.as_str())
        .execute(db_pool)
        .await
        .map_err(|e| format!("Failed to update artist for track {}: {}", track_id.as_str(), e))?;
    Ok(())
}

pub async fn update_track_origin(
    db_pool: &SqlitePool,
    track_id: &VideoId,
    origin_id: i64,
) -> Result<(), Error> {
    sqlx::query("UPDATE tracks SET origin_id = ?1 WHERE id = ?2")
        .bind(origin_id)
        .bind(track_id.as_str())
        .execute(db_pool)
        .await
        .map_err(|e| format!("Failed to update origin for track {}: {}", track_id.as_str(), e))?;
    Ok(())
}