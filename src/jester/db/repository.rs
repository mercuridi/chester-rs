use anyhow::{Context, Result, anyhow};
use serde_json::Value;
use sqlx::{FromRow, SqliteConnection, SqlitePool};

use crate::{
    jester::db::metadata::MetadataKind,
    jester::track::types::{TrackInfo, VideoId},
};

const TAXONOMY_SUMMARY: &str = "TRIM(COALESCE(tracks.mood, '') || CASE WHEN tracks.intensity IS NULL THEN '' ELSE ', ' || tracks.intensity END || CASE WHEN tracks.function_tag IS NULL THEN '' ELSE ', ' || tracks.function_tag END || CASE WHEN EXISTS (SELECT 1 FROM track_environments WHERE track_id = tracks.id) THEN ', ' || (SELECT GROUP_CONCAT(environment, ', ') FROM track_environments WHERE track_id = tracks.id) ELSE '' END, ', ')";

#[derive(Clone, Debug, PartialEq, Eq, FromRow)]
pub struct LibraryTrack {
    pub title: String,
    pub artist: Option<String>,
    pub origin: Option<String>,
    pub taxonomy: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, FromRow)]
pub struct LibraryGroupEntry {
    pub group_name: Option<String>,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq, FromRow)]
pub struct TrackSearchResult {
    pub id: String,
    pub title: String,
    pub artist: Option<String>,
    pub origin: Option<String>,
    pub taxonomy: Option<String>,
}

#[allow(dead_code)]
pub async fn get_or_insert_metadata_id(
    db_pool: &SqlitePool,
    kind: MetadataKind,
    value: &str,
) -> Result<i64> {
    sqlx::query(kind.upsert_sql())
        .bind(value)
        .execute(db_pool)
        .await
        .context("Database metadata upsert failed")?;

    sqlx::query_scalar::<_, i64>(kind.select_sql())
        .bind(value)
        .fetch_one(db_pool)
        .await
        .context("Database metadata lookup failed")
}

async fn get_or_insert_metadata_id_on_connection(
    connection: &mut SqliteConnection,
    kind: MetadataKind,
    value: &str,
) -> Result<i64> {
    sqlx::query(kind.upsert_sql())
        .bind(value)
        .execute(&mut *connection)
        .await
        .context("Database metadata upsert failed")?;

    sqlx::query_scalar::<_, i64>(kind.select_sql())
        .bind(value)
        .fetch_one(&mut *connection)
        .await
        .context("Database metadata lookup failed")
}

#[allow(dead_code)]
pub async fn insert_new_track(
    db_pool: &SqlitePool,
    video_id: &VideoId,
    slim: &serde_json::Value,
    title: &str,
    artist_id: i64,
    origin_id: i64,
) -> Result<()> {
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

/// Persist a downloaded track and its metadata as one database operation.
pub async fn insert_new_track_with_metadata(
    db_pool: &SqlitePool,
    video_id: &VideoId,
    slim: &serde_json::Value,
    title: &str,
    artist: Option<&str>,
    origin: Option<&str>,
) -> Result<()> {
    let mut transaction = db_pool
        .begin()
        .await
        .context("Failed to begin track insert transaction")?;

    let artist_id = match artist {
        Some(value) => Some(
            get_or_insert_metadata_id_on_connection(&mut transaction, MetadataKind::Artist, value)
                .await?,
        ),
        None => None,
    };
    let origin_id = match origin {
        Some(value) => Some(
            get_or_insert_metadata_id_on_connection(&mut transaction, MetadataKind::Origin, value)
                .await?,
        ),
        None => None,
    };

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
    .execute(&mut *transaction)
    .await
    .context("Failed to insert track")?;

    transaction
        .commit()
        .await
        .context("Failed to commit track insert transaction")?;

    Ok(())
}

pub async fn fetch_library_all(db_pool: &SqlitePool) -> Result<Vec<LibraryTrack>> {
    sqlx::query_as(&format!(
        "SELECT tracks.track_title AS title, artists.artist, origins.origin,
                NULLIF({TAXONOMY_SUMMARY}, '') AS taxonomy
         FROM tracks
         LEFT JOIN artists ON tracks.artist_id = artists.id
         LEFT JOIN origins ON tracks.origin_id = origins.id
         ORDER BY tracks.track_title"
    ))
    .fetch_all(db_pool)
    .await
    .context("Library query failed")
}

pub async fn fetch_library_by_artist(db_pool: &SqlitePool) -> Result<Vec<LibraryGroupEntry>> {
    sqlx::query_as(
        "SELECT artists.artist AS group_name, tracks.track_title AS title
         FROM tracks
         LEFT JOIN artists ON tracks.artist_id = artists.id
         ORDER BY artists.artist IS NULL, artists.artist",
    )
    .fetch_all(db_pool)
    .await
    .context("Artist library query failed")
}

pub async fn fetch_library_by_origin(db_pool: &SqlitePool) -> Result<Vec<LibraryGroupEntry>> {
    sqlx::query_as(
        "SELECT origins.origin AS group_name, tracks.track_title AS title
         FROM tracks
         LEFT JOIN origins ON tracks.origin_id = origins.id
         ORDER BY origins.origin IS NULL, origins.origin",
    )
    .fetch_all(db_pool)
    .await
    .context("Origin library query failed")
}

pub async fn fetch_library_by_tag(db_pool: &SqlitePool) -> Result<Vec<LibraryGroupEntry>> {
    sqlx::query_as(
        "SELECT tag AS group_name, track_title AS title FROM (
             SELECT mood AS tag, track_title FROM tracks WHERE mood IS NOT NULL
             UNION ALL SELECT intensity, track_title FROM tracks WHERE intensity IS NOT NULL
             UNION ALL SELECT function_tag, track_title FROM tracks WHERE function_tag IS NOT NULL
             UNION ALL SELECT texture, tracks.track_title FROM track_textures JOIN tracks ON tracks.id = track_textures.track_id
             UNION ALL SELECT environment, tracks.track_title FROM track_environments JOIN tracks ON tracks.id = track_environments.track_id
             UNION ALL SELECT label, tracks.track_title FROM track_labels JOIN tracks ON tracks.id = track_labels.track_id
             UNION ALL SELECT 'Unclassified', track_title FROM tracks
                WHERE mood IS NULL AND intensity IS NULL AND function_tag IS NULL
                   AND NOT EXISTS (SELECT 1 FROM track_textures WHERE track_id = tracks.id)
                   AND NOT EXISTS (SELECT 1 FROM track_environments WHERE track_id = tracks.id)
                   AND NOT EXISTS (SELECT 1 FROM track_labels WHERE track_id = tracks.id)
         ) ORDER BY CASE WHEN tag = 'Unclassified' THEN 1 ELSE 0 END, tag, track_title",
    ).fetch_all(db_pool).await.context("Taxonomy library query failed")
}

pub async fn fetch_library_by_incomplete(db_pool: &SqlitePool) -> Result<Vec<LibraryTrack>> {
    sqlx::query_as(
        "SELECT tracks.track_title AS title, artists.artist, origins.origin, NULL AS taxonomy
            FROM tracks
            LEFT JOIN artists ON tracks.artist_id = artists.id
            LEFT JOIN origins ON tracks.origin_id = origins.id
            WHERE tracks.artist_id IS NULL OR tracks.origin_id IS NULL
            ORDER BY artists.artist, origins.origin, tracks.track_title",
    )
    .fetch_all(db_pool)
    .await
    .context("Incomplete library query failed")
}

pub async fn lookup_track(db_pool: &SqlitePool, video_id: &VideoId) -> Result<Option<TrackInfo>> {
    let result: Option<(String, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT tracks.id, tracks.track_title,
                artists.artist,
                origins.origin
         FROM tracks
         LEFT JOIN artists ON tracks.artist_id = artists.id
         LEFT JOIN origins ON tracks.origin_id = origins.id
         WHERE tracks.id = ?1",
    )
    .bind(video_id.as_str())
    .fetch_optional(db_pool)
    .await
    .context("Track lookup failed")?;

    Ok(result.map(|(id, title, artist, origin)| TrackInfo {
        id: VideoId::from(id),
        title,
        artist,
        origin,
    }))
}

pub async fn require_track(db_pool: &SqlitePool, id: &VideoId) -> Result<TrackInfo> {
    lookup_track(db_pool, id)
        .await?
        .ok_or_else(|| anyhow!("Track could not be found in the database."))
}

pub async fn search_metadata(
    db_pool: &SqlitePool,
    kind: MetadataKind,
    needle: &str,
    limit: i64,
) -> Result<Vec<String>> {
    let query = match kind {
        MetadataKind::Artist => {
            "SELECT DISTINCT artist FROM artists WHERE LOWER(artist) LIKE ?1 LIMIT ?2"
        }
        MetadataKind::Origin => {
            "SELECT DISTINCT origin FROM origins WHERE LOWER(origin) LIKE ?1 LIMIT ?2"
        }
    };

    sqlx::query_scalar(query)
        .bind(format!("%{needle}%"))
        .bind(limit)
        .fetch_all(db_pool)
        .await
        .context("Autocomplete metadata query failed")
}

pub async fn search_labels(db_pool: &SqlitePool, needle: &str, limit: i64) -> Result<Vec<String>> {
    sqlx::query_scalar(
        "SELECT DISTINCT label
         FROM track_labels
         WHERE LOWER(label) LIKE LOWER(?1)
         ORDER BY label
         LIMIT ?2",
    )
    .bind(format!("%{needle}%"))
    .bind(limit)
    .fetch_all(db_pool)
    .await
    .context("Autocomplete label query failed")
}

pub async fn search_tracks(
    db_pool: &SqlitePool,
    needle: &str,
    limit: i64,
) -> Result<Vec<TrackSearchResult>> {
    sqlx::query_as(&format!(
        "SELECT DISTINCT tracks.id, tracks.track_title AS title, artists.artist, origins.origin,
                NULLIF({TAXONOMY_SUMMARY}, '') AS taxonomy
         FROM tracks
         LEFT JOIN artists ON tracks.artist_id = artists.id
         LEFT JOIN origins ON tracks.origin_id = origins.id
         WHERE LOWER(tracks.track_title) LIKE ?1
            OR LOWER(COALESCE(artists.artist, '')) LIKE ?1
            OR LOWER(COALESCE(origins.origin, '')) LIKE ?1
            OR LOWER(COALESCE(tracks.mood, '')) LIKE ?1
            OR LOWER(COALESCE(tracks.intensity, '')) LIKE ?1
            OR LOWER(COALESCE(tracks.function_tag, '')) LIKE ?1
            OR EXISTS (SELECT 1 FROM track_textures WHERE track_id = tracks.id AND LOWER(texture) LIKE ?1)
            OR EXISTS (SELECT 1 FROM track_environments WHERE track_id = tracks.id AND LOWER(environment) LIKE ?1)
            OR EXISTS (SELECT 1 FROM track_labels WHERE track_id = tracks.id AND LOWER(label) LIKE ?1)
         LIMIT ?2"
    ))
    .bind(format!("%{needle}%"))
    .bind(limit)
    .fetch_all(db_pool)
    .await
    .context("Autocomplete track query failed")
}

pub async fn search_incomplete_tracks(
    db_pool: &SqlitePool,
    needle: &str,
    limit: i64,
) -> Result<Vec<TrackSearchResult>> {
    sqlx::query_as(
        "SELECT DISTINCT tracks.id, tracks.track_title AS title, artists.artist, origins.origin,
                NULL AS taxonomy
         FROM tracks
         LEFT JOIN artists ON tracks.artist_id = artists.id
         LEFT JOIN origins ON tracks.origin_id = origins.id
         WHERE (tracks.artist_id IS NULL OR tracks.origin_id IS NULL)
           AND (LOWER(tracks.track_title) LIKE ?1
            OR LOWER(COALESCE(artists.artist, '')) LIKE ?1
            OR LOWER(COALESCE(origins.origin, '')) LIKE ?1)
         GROUP BY tracks.id
         LIMIT ?2",
    )
    .bind(format!("%{needle}%"))
    .bind(limit)
    .fetch_all(db_pool)
    .await
    .context("Incomplete track search query failed")
}

pub async fn clear_track_taxonomy(db_pool: &SqlitePool, track_id: &VideoId) -> Result<()> {
    let mut transaction = db_pool
        .begin()
        .await
        .context("Failed to begin taxonomy reset transaction")?;

    sqlx::query(
        "UPDATE tracks SET mood = NULL, intensity = NULL, function_tag = NULL WHERE id = ?1",
    )
    .bind(track_id.as_str())
    .execute(&mut *transaction)
    .await
    .with_context(|| format!("Failed to clear taxonomy for track {}", track_id.as_str()))?;
    sqlx::query("DELETE FROM track_textures WHERE track_id = ?1")
        .bind(track_id.as_str())
        .execute(&mut *transaction)
        .await
        .with_context(|| format!("Failed to clear textures for track {}", track_id.as_str()))?;
    sqlx::query("DELETE FROM track_environments WHERE track_id = ?1")
        .bind(track_id.as_str())
        .execute(&mut *transaction)
        .await
        .with_context(|| {
            format!(
                "Failed to clear environments for track {}",
                track_id.as_str()
            )
        })?;
    sqlx::query("DELETE FROM track_labels WHERE track_id = ?1")
        .bind(track_id.as_str())
        .execute(&mut *transaction)
        .await
        .with_context(|| format!("Failed to clear labels for track {}", track_id.as_str()))?;

    transaction
        .commit()
        .await
        .context("Failed to commit taxonomy reset transaction")?;

    Ok(())
}

pub async fn set_track_taxonomy(
    db_pool: &SqlitePool,
    track_id: &VideoId,
    mood: &str,
    intensity: &str,
    function_tag: Option<&str>,
) -> Result<()> {
    sqlx::query("UPDATE tracks SET mood = ?1, intensity = ?2, function_tag = ?3 WHERE id = ?4")
        .bind(mood)
        .bind(intensity)
        .bind(function_tag)
        .bind(track_id.as_str())
        .execute(db_pool)
        .await
        .with_context(|| format!("Failed to set taxonomy for track {}", track_id.as_str()))?;
    Ok(())
}

pub async fn insert_track_texture(
    db_pool: &SqlitePool,
    track_id: &VideoId,
    texture: &str,
) -> Result<()> {
    sqlx::query("INSERT OR IGNORE INTO track_textures (track_id, texture) VALUES (?1, ?2)")
        .bind(track_id.as_str())
        .bind(texture)
        .execute(db_pool)
        .await
        .with_context(|| format!("Failed to add texture to track {}", track_id.as_str()))?;
    Ok(())
}

pub async fn insert_track_environment(
    db_pool: &SqlitePool,
    track_id: &VideoId,
    environment: &str,
) -> Result<()> {
    sqlx::query("INSERT OR IGNORE INTO track_environments (track_id, environment) VALUES (?1, ?2)")
        .bind(track_id.as_str())
        .bind(environment)
        .execute(db_pool)
        .await
        .with_context(|| format!("Failed to add environment to track {}", track_id.as_str()))?;
    Ok(())
}

pub async fn insert_track_label(
    db_pool: &SqlitePool,
    track_id: &VideoId,
    label: &str,
) -> Result<()> {
    sqlx::query("INSERT OR IGNORE INTO track_labels (track_id, label) VALUES (?1, ?2)")
        .bind(track_id.as_str())
        .bind(label)
        .execute(db_pool)
        .await
        .with_context(|| format!("Failed to add label to track {}", track_id.as_str()))?;
    Ok(())
}

/// Update any combination of track title, artist, and origin atomically.
pub async fn update_track_metadata(
    db_pool: &SqlitePool,
    track_id: &VideoId,
    title: Option<&str>,
    artist: Option<&str>,
    origin: Option<&str>,
) -> Result<()> {
    let mut transaction = db_pool
        .begin()
        .await
        .context("Failed to begin track metadata transaction")?;

    let artist_id = match artist {
        Some(value) => Some(
            get_or_insert_metadata_id_on_connection(&mut transaction, MetadataKind::Artist, value)
                .await?,
        ),
        None => None,
    };
    let origin_id = match origin {
        Some(value) => Some(
            get_or_insert_metadata_id_on_connection(&mut transaction, MetadataKind::Origin, value)
                .await?,
        ),
        None => None,
    };

    sqlx::query(
        "UPDATE tracks
         SET track_title = COALESCE(?1, track_title),
             artist_id = COALESCE(?2, artist_id),
             origin_id = COALESCE(?3, origin_id)
         WHERE id = ?4",
    )
    .bind(title)
    .bind(artist_id)
    .bind(origin_id)
    .bind(track_id.as_str())
    .execute(&mut *transaction)
    .await
    .with_context(|| format!("Failed to update metadata for track {}", track_id.as_str()))?;

    transaction
        .commit()
        .await
        .context("Failed to commit track metadata transaction")?;

    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    type TestResult<T = ()> = std::result::Result<T, crate::discord::context::Error>;

    async fn test_pool() -> TestResult<(tempfile::TempDir, SqlitePool)> {
        let directory = tempdir()?;
        let database_url = format!("sqlite://{}", directory.path().join("jester.db").display());
        let pool = crate::database::open_sqlite_pool(&database_url, "test").await?;
        crate::jester::db::schema::initialise(&pool).await?;
        Ok((directory, pool))
    }

    async fn add_track(
        pool: &SqlitePool,
        id: &str,
        title: &str,
        artist: &str,
        origin: &str,
    ) -> TestResult {
        let artist_id = get_or_insert_metadata_id(pool, MetadataKind::Artist, artist).await?;
        let origin_id = get_or_insert_metadata_id(pool, MetadataKind::Origin, origin).await?;
        insert_new_track(
            pool,
            &VideoId::from(id),
            &serde_json::json!({ "upload_date": "2026-01-01", "title": "YouTube title" }),
            title,
            artist_id,
            origin_id,
        )
        .await?;
        Ok(())
    }

    #[tokio::test]
    async fn metadata_ids_are_reused_and_tracks_round_trip() -> TestResult {
        let (_directory, pool) = test_pool().await?;
        let first_artist =
            get_or_insert_metadata_id(&pool, MetadataKind::Artist, "The Band").await?;
        let second_artist =
            get_or_insert_metadata_id(&pool, MetadataKind::Artist, "The Band").await?;
        assert_eq!(first_artist, second_artist);

        add_track(&pool, "video-1", "Track title", "The Band", "Album").await?;
        let track = lookup_track(&pool, &VideoId::from("video-1"))
            .await?
            .ok_or_else(|| std::io::Error::other("inserted track should be found"))?;
        assert_eq!(track.title, "Track title");
        assert_eq!(track.artist.as_deref(), Some("The Band"));
        assert_eq!(track.origin.as_deref(), Some("Album"));
        assert!(
            lookup_track(&pool, &VideoId::from("missing"))
                .await?
                .is_none()
        );
        assert!(
            require_track(&pool, &VideoId::from("missing"))
                .await
                .is_err()
        );
        Ok(())
    }

    #[tokio::test]
    async fn metadata_creation_is_safe_for_concurrent_callers() -> TestResult {
        let (_directory, pool) = test_pool().await?;

        let artist_ids =
            futures::future::join_all((0..16).map(|_| {
                get_or_insert_metadata_id(&pool, MetadataKind::Artist, "Concurrent Artist")
            }))
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
        let origin_ids =
            futures::future::join_all((0..16).map(|_| {
                get_or_insert_metadata_id(&pool, MetadataKind::Origin, "Concurrent Origin")
            }))
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;

        assert!(artist_ids.windows(2).all(|ids| ids[0] == ids[1]));
        assert!(origin_ids.windows(2).all(|ids| ids[0] == ids[1]));
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM artists WHERE artist = 'Concurrent Artist'",
            )
            .fetch_one(&pool)
            .await?,
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM origins WHERE origin = 'Concurrent Origin'",
            )
            .fetch_one(&pool)
            .await?,
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn track_creation_rolls_back_metadata_when_track_insert_fails() -> TestResult {
        let (_directory, pool) = test_pool().await?;
        add_track(
            &pool,
            "existing-track",
            "Existing",
            "Existing Artist",
            "Existing Origin",
        )
        .await?;

        let error = insert_new_track_with_metadata(
            &pool,
            &VideoId::from("existing-track"),
            &serde_json::json!({
                "upload_date": "2026-01-01",
                "title": "Duplicate"
            }),
            "Duplicate",
            Some("Rolled Back Artist"),
            Some("Rolled Back Origin"),
        )
        .await
        .unwrap_err();

        assert!(format!("{error:?}").contains("UNIQUE"));
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM artists WHERE artist = 'Rolled Back Artist'",
            )
            .fetch_one(&pool)
            .await?,
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM origins WHERE origin = 'Rolled Back Origin'",
            )
            .fetch_one(&pool)
            .await?,
            0
        );
        assert_eq!(
            require_track(&pool, &VideoId::from("existing-track"))
                .await?
                .title,
            "Existing"
        );
        Ok(())
    }

    #[tokio::test]
    async fn library_views_and_taxonomy_updates_return_expected_rows() -> TestResult {
        let (_directory, pool) = test_pool().await?;
        add_track(&pool, "video-1", "Alpha", "Artist A", "Origin A").await?;
        insert_new_track_with_metadata(
            &pool,
            &VideoId::from("video-2"),
            &serde_json::json!({}),
            "Beta",
            None,
            Some("Origin B"),
        )
        .await?;
        let track_id = VideoId::from("video-1");
        set_track_taxonomy(
            &pool,
            &track_id,
            "whimsical",
            "subtle",
            Some("investigative"),
        )
        .await?;
        insert_track_texture(&pool, &track_id, "synthetic").await?;
        insert_track_environment(&pool, &track_id, "forest").await?;
        insert_track_label(&pool, &track_id, "live").await?;

        assert_eq!(
            fetch_library_all(&pool).await?,
            vec![
                LibraryTrack {
                    title: "Alpha".into(),
                    artist: Some("Artist A".into()),
                    origin: Some("Origin A".into()),
                    taxonomy: Some("whimsical, subtle, investigative, forest".into())
                },
                LibraryTrack {
                    title: "Beta".into(),
                    artist: None,
                    origin: Some("Origin B".into()),
                    taxonomy: None
                },
            ]
        );
        assert_eq!(
            fetch_library_by_artist(&pool).await?[0],
            LibraryGroupEntry {
                group_name: Some("Artist A".into()),
                title: "Alpha".into()
            }
        );
        assert_eq!(
            fetch_library_by_origin(&pool).await?[0],
            LibraryGroupEntry {
                group_name: Some("Origin A".into()),
                title: "Alpha".into()
            }
        );
        assert_eq!(
            fetch_library_by_tag(&pool).await?,
            vec![
                LibraryGroupEntry {
                    group_name: Some("forest".into()),
                    title: "Alpha".into()
                },
                LibraryGroupEntry {
                    group_name: Some("investigative".into()),
                    title: "Alpha".into()
                },
                LibraryGroupEntry {
                    group_name: Some("live".into()),
                    title: "Alpha".into()
                },
                LibraryGroupEntry {
                    group_name: Some("subtle".into()),
                    title: "Alpha".into()
                },
                LibraryGroupEntry {
                    group_name: Some("synthetic".into()),
                    title: "Alpha".into()
                },
                LibraryGroupEntry {
                    group_name: Some("whimsical".into()),
                    title: "Alpha".into()
                },
                LibraryGroupEntry {
                    group_name: Some("Unclassified".into()),
                    title: "Beta".into()
                }
            ]
        );
        assert_eq!(
            fetch_library_by_incomplete(&pool).await?,
            vec![LibraryTrack {
                title: "Beta".into(),
                artist: None,
                origin: Some("Origin B".into()),
                taxonomy: None
            }]
        );

        assert_clearing_taxonomy_updates_tags(&pool, &track_id).await?;
        Ok(())
    }

    async fn assert_clearing_taxonomy_updates_tags(
        pool: &SqlitePool,
        track_id: &VideoId,
    ) -> TestResult {
        clear_track_taxonomy(pool, track_id).await?;
        assert_eq!(
            fetch_library_by_tag(pool).await?[0],
            LibraryGroupEntry {
                group_name: Some("Unclassified".into()),
                title: "Alpha".into()
            }
        );
        Ok(())
    }

    #[tokio::test]
    async fn taxonomy_reset_rolls_back_when_a_later_delete_fails() -> TestResult {
        let (_directory, pool) = test_pool().await?;
        add_track(&pool, "rollback-track", "Rollback", "Artist", "Origin").await?;
        let track_id = VideoId::from("rollback-track");
        set_track_taxonomy(
            &pool,
            &track_id,
            "whimsical",
            "subtle",
            Some("investigative"),
        )
        .await?;
        insert_track_texture(&pool, &track_id, "synthetic").await?;
        insert_track_environment(&pool, &track_id, "forest").await?;
        insert_track_label(&pool, &track_id, "keep-me").await?;

        sqlx::query(
            "CREATE TRIGGER fail_environment_delete
             BEFORE DELETE ON track_environments
             BEGIN
                 SELECT RAISE(ABORT, 'forced taxonomy reset failure');
             END",
        )
        .execute(&pool)
        .await?;

        assert!(clear_track_taxonomy(&pool, &track_id).await.is_err());

        let taxonomy: (Option<String>, Option<String>, Option<String>) =
            sqlx::query_as("SELECT mood, intensity, function_tag FROM tracks WHERE id = ?1")
                .bind(track_id.as_str())
                .fetch_one(&pool)
                .await?;
        assert_eq!(
            taxonomy,
            (
                Some("whimsical".into()),
                Some("subtle".into()),
                Some("investigative".into())
            )
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM track_textures WHERE track_id = ?1",
            )
            .bind(track_id.as_str())
            .fetch_one(&pool)
            .await?,
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM track_environments WHERE track_id = ?1",
            )
            .bind(track_id.as_str())
            .fetch_one(&pool)
            .await?,
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM track_labels WHERE track_id = ?1")
                .bind(track_id.as_str())
                .fetch_one(&pool)
                .await?,
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn searches_are_case_insensitive_limited_and_include_taxonomy() -> TestResult {
        let (_directory, pool) = test_pool().await?;
        add_track(&pool, "video-1", "Northern Lights", "Aurora", "Winter").await?;
        insert_new_track_with_metadata(
            &pool,
            &VideoId::from("video-2"),
            &serde_json::json!({}),
            "Summer Sun",
            Some("Sol"),
            None,
        )
        .await?;
        let track_id = VideoId::from("video-1");
        set_track_taxonomy(&pool, &track_id, "mysterious", "subtle", None).await?;
        insert_track_texture(&pool, &track_id, "ambient").await?;
        insert_track_environment(&pool, &track_id, "tundra").await?;

        assert_eq!(
            search_metadata(&pool, MetadataKind::Artist, "aur", 1).await?,
            ["Aurora"]
        );
        let tagged = search_tracks(&pool, "AMBI", 10).await?;
        assert_eq!(tagged.len(), 1);
        assert_eq!(tagged[0].id, "video-1");
        assert_eq!(
            tagged[0].taxonomy.as_deref(),
            Some("mysterious, subtle, tundra")
        );
        let environmental = search_tracks(&pool, "TUND", 10).await?;
        assert_eq!(environmental[0].id, "video-1");
        let incomplete = search_incomplete_tracks(&pool, "summer", 1).await?;
        assert_eq!(incomplete.len(), 1);
        assert_eq!(incomplete[0].id, "video-2");
        Ok(())
    }

    #[tokio::test]
    async fn updates_change_only_the_requested_track_metadata() -> TestResult {
        let (_directory, pool) = test_pool().await?;
        add_track(&pool, "video-1", "Before", "Artist A", "Origin A").await?;
        let track_id = VideoId::from("video-1");
        update_track_metadata(
            &pool,
            &track_id,
            Some("After"),
            Some("Artist B"),
            Some("Origin B"),
        )
        .await?;

        let track = require_track(&pool, &track_id).await?;
        assert_eq!(
            (track.title, track.artist, track.origin),
            (
                "After".into(),
                Some("Artist B".into()),
                Some("Origin B".into())
            )
        );
        Ok(())
    }
}
