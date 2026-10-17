use std::collections::HashSet;

use anyhow::{Context, Result, anyhow};
use sqlx::SqlitePool;

use crate::jester::{
    db::normalize_tag,
    track::{TrackInfo, VideoId},
};

pub const MIX_LIMIT: usize = 25;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MixFilter {
    /// A track is included when it has any one of these tags.
    pub tags: Vec<String>,
}

pub fn parse_filter(input: Option<&str>) -> Result<Vec<String>> {
    let mut tags = Vec::new();
    for raw in input.unwrap_or_default().split(',') {
        if raw.trim().is_empty() {
            continue;
        }
        let tag = normalize_tag(raw)?;
        if !tags.contains(&tag) {
            tags.push(tag);
        }
    }
    Ok(tags)
}

pub async fn fetch_mix_tracks(
    db_pool: &SqlitePool,
    filter: &MixFilter,
    limit: usize,
) -> Result<Vec<TrackInfo>> {
    if !filter.tags.is_empty() {
        let known_tags: HashSet<String> = sqlx::query_scalar("SELECT tag FROM tags")
            .fetch_all(db_pool)
            .await
            .context("Failed to validate mix tags")?
            .into_iter()
            .collect();
        if let Some(unknown) = filter.tags.iter().find(|tag| !known_tags.contains(*tag)) {
            return Err(anyhow!(
                "Unknown tag `{unknown}`. Choose a tag from autocomplete."
            ));
        }
    }

    let mut sql = String::from(
        "SELECT tracks.id, tracks.track_title, artists.artist, origins.origin
         FROM tracks
         LEFT JOIN artists ON tracks.artist_id = artists.id
         LEFT JOIN origins ON tracks.origin_id = origins.id
         WHERE 1 = 1",
    );
    let mut values = Vec::new();
    if !filter.tags.is_empty() {
        sql.push_str(" AND (");
        for (index, tag) in filter.tags.iter().enumerate() {
            if index > 0 {
                sql.push_str(" OR ");
            }
            sql.push_str(
                "EXISTS (SELECT 1 FROM track_tags
                 WHERE track_id = tracks.id AND tag = ?)",
            );
            values.push(tag.clone());
        }
        sql.push(')');
    }
    sql.push_str(" ORDER BY RANDOM() LIMIT ?");

    let mut query = sqlx::query_as::<_, (String, String, Option<String>, Option<String>)>(&sql);
    for value in values {
        query = query.bind(value);
    }
    query = query.bind(i64::try_from(limit).map_err(|_| anyhow!("Mix size is too large."))?);

    Ok(query
        .fetch_all(db_pool)
        .await
        .context("Mix query failed")?
        .into_iter()
        .map(|(id, title, artist, origin)| TrackInfo {
            id: VideoId::from(id),
            title,
            artist,
            origin,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::parse_filter;

    #[test]
    fn parses_normalized_tag_lists_and_removes_duplicates() {
        assert_eq!(
            parse_filter(Some("  Dark   Forest, eerie, dark forest ")).expect("valid tags"),
            vec!["dark forest".to_owned(), "eerie".to_owned()]
        );
    }

    #[test]
    fn ignores_empty_filter_parts() {
        assert_eq!(
            parse_filter(Some(", ,")).expect("empty filters"),
            Vec::<String>::new()
        );
    }
}
