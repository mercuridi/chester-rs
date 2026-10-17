use anyhow::{Context, Result, anyhow};
use sqlx::SqlitePool;

use crate::jester::track::VideoId;

/// Canonicalize a tag by trimming, collapsing whitespace, and lowercasing.
pub fn normalize_tag(raw: &str) -> Result<String> {
    let tag = raw
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    if tag.is_empty() {
        return Err(anyhow!("A tag cannot be empty."));
    }
    if tag.chars().count() > 100 {
        return Err(anyhow!("Tags must be 100 characters or fewer."));
    }
    if tag.contains(',') {
        return Err(anyhow!(
            "Tags cannot contain commas because commas separate mix selections."
        ));
    }
    Ok(tag)
}

pub async fn create_tag(db_pool: &SqlitePool, raw: &str) -> Result<String> {
    let tag = normalize_tag(raw)?;
    sqlx::query("INSERT OR IGNORE INTO tags (tag) VALUES (?1)")
        .bind(&tag)
        .execute(db_pool)
        .await
        .context("Failed to add tag to the vocabulary")?;
    Ok(tag)
}

pub async fn list_tags(db_pool: &SqlitePool, partial: &str, limit: i64) -> Result<Vec<String>> {
    let needle = format!("%{}%", partial.trim().to_lowercase());
    sqlx::query_scalar("SELECT tag FROM tags WHERE tag LIKE ?1 ORDER BY tag LIMIT ?2")
        .bind(needle)
        .bind(limit)
        .fetch_all(db_pool)
        .await
        .context("Failed to list tags")
}

pub async fn add_track_tag(db_pool: &SqlitePool, track_id: &VideoId, raw: &str) -> Result<String> {
    let tag = normalize_tag(raw)?;
    let result = sqlx::query(
        "INSERT OR IGNORE INTO track_tags (track_id, tag)
         SELECT ?1, tag FROM tags WHERE tag = ?2 COLLATE NOCASE",
    )
    .bind(track_id.as_str())
    .bind(&tag)
    .execute(db_pool)
    .await
    .context("Failed to apply tag to track")?;
    if result.rows_affected() == 0
        && !sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM tags WHERE tag = ?1 COLLATE NOCASE)",
        )
        .bind(&tag)
        .fetch_one(db_pool)
        .await
        .context("Failed to validate tag")?
    {
        return Err(anyhow!(
            "Unknown tag `{tag}`. Choose a tag from autocomplete."
        ));
    }
    Ok(tag)
}

pub async fn remove_track_tag(
    db_pool: &SqlitePool,
    track_id: &VideoId,
    raw: &str,
) -> Result<String> {
    let tag = normalize_tag(raw)?;
    let result =
        sqlx::query("DELETE FROM track_tags WHERE track_id = ?1 AND tag = ?2 COLLATE NOCASE")
            .bind(track_id.as_str())
            .bind(&tag)
            .execute(db_pool)
            .await
            .context("Failed to remove tag from track")?;
    if result.rows_affected() == 0 {
        return Err(anyhow!("Tag `{tag}` was not applied to this track."));
    }
    Ok(tag)
}
