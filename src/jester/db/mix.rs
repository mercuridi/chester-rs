use sqlx::SqlitePool;

use crate::{
    discord::context::Error,
    jester::{
        db::taxonomy::{ENVIRONMENTS, FUNCTIONS, INTENSITIES, MOODS, TEXTURES},
        track::types::{TrackInfo, VideoId},
    },
};

pub const MIX_LIMIT: usize = 25;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MixTag {
    Mood(String),
    Intensity(String),
    Function(String),
    Texture(String),
    Environment(String),
    Label(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MixFilter {
    pub include: Vec<MixTag>,
    pub exclude: Vec<MixTag>,
}

pub fn parse_filter(input: Option<&str>) -> Result<Vec<MixTag>, Error> {
    input
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(parse_tag)
        .collect()
}

pub fn parse_tag(raw: &str) -> Result<MixTag, Error> {
    let (namespace, value) = raw
        .split_once('=')
        .map_or((None, raw), |(namespace, value)| (Some(namespace), value));
    let value = value.trim().to_lowercase();
    if value.is_empty() {
        return Err(format!("Empty mix tag in `{raw}`.").into());
    }

    let tag = match namespace.map(str::trim) {
        Some("mood") if MOODS.contains(&value.as_str()) => MixTag::Mood(value),
        Some("intensity") if INTENSITIES.contains(&value.as_str()) => MixTag::Intensity(value),
        Some("function") if FUNCTIONS.contains(&value.as_str()) => MixTag::Function(value),
        Some("texture") if TEXTURES.contains(&value.as_str()) => MixTag::Texture(value),
        Some("environment") if ENVIRONMENTS.contains(&value.as_str()) => MixTag::Environment(value),
        Some("label") => MixTag::Label(value),
        Some(_namespace) => {
            return Err(format!(
                "Unknown or invalid mix tag `{raw}`. Use mood, intensity, function, texture, environment, or label."
            )
            .into());
        }
        None if MOODS.contains(&value.as_str()) => MixTag::Mood(value),
        None if INTENSITIES.contains(&value.as_str()) => MixTag::Intensity(value),
        None if FUNCTIONS.contains(&value.as_str()) => MixTag::Function(value),
        None if TEXTURES.contains(&value.as_str()) => MixTag::Texture(value),
        None if ENVIRONMENTS.contains(&value.as_str()) => MixTag::Environment(value),
        None => {
            return Err(format!(
                "Unknown mix tag `{raw}`. Custom labels must use the `label=` prefix."
            )
            .into());
        }
    };

    Ok(tag)
}

pub async fn fetch_mix_tracks(
    db_pool: &SqlitePool,
    filter: &MixFilter,
    limit: usize,
) -> Result<Vec<TrackInfo>, Error> {
    let mut sql = String::from(
        "SELECT tracks.id, tracks.track_title, artists.artist, origins.origin
         FROM tracks
         LEFT JOIN artists ON tracks.artist_id = artists.id
         LEFT JOIN origins ON tracks.origin_id = origins.id
         WHERE 1 = 1",
    );
    let mut values = Vec::new();

    for tag in &filter.include {
        sql.push_str(" AND ");
        append_condition(&mut sql, &mut values, tag, false);
    }
    for tag in &filter.exclude {
        sql.push_str(" AND ");
        append_condition(&mut sql, &mut values, tag, true);
    }
    sql.push_str(" ORDER BY RANDOM() LIMIT ?");

    let mut query = sqlx::query_as::<_, (String, String, String, String)>(&sql);
    for value in values {
        query = query.bind(value);
    }
    query = query.bind(i64::try_from(limit).map_err(|_| "Mix size is too large.")?);

    Ok(query
        .fetch_all(db_pool)
        .await
        .map_err(|error| format!("Mix query failed: {error}"))?
        .into_iter()
        .map(|(id, title, artist, origin)| TrackInfo {
            id: VideoId::from(id),
            title,
            artist,
            origin,
        })
        .collect())
}

fn append_condition(sql: &mut String, values: &mut Vec<String>, tag: &MixTag, exclude: bool) {
    match tag {
        MixTag::Mood(value) => append_scalar(sql, values, "tracks.mood", value, exclude),
        MixTag::Intensity(value) => append_scalar(sql, values, "tracks.intensity", value, exclude),
        MixTag::Function(value) => {
            append_scalar(sql, values, "tracks.function_tag", value, exclude)
        }
        MixTag::Texture(value) => {
            append_exists(sql, values, "track_textures", "texture", value, exclude)
        }
        MixTag::Environment(value) => {
            append_exists(
                sql,
                values,
                "track_environments",
                "environment",
                value,
                exclude,
            );
        }
        MixTag::Label(value) => append_exists(sql, values, "track_labels", "label", value, exclude),
    }
}

fn append_scalar(
    sql: &mut String,
    values: &mut Vec<String>,
    column: &str,
    value: &str,
    exclude: bool,
) {
    if exclude {
        sql.push_str(&format!("COALESCE({column}, '') <> ?"));
    } else {
        sql.push_str(&format!("{column} = ?"));
    }
    values.push(value.to_string());
}

fn append_exists(
    sql: &mut String,
    values: &mut Vec<String>,
    table: &str,
    column: &str,
    value: &str,
    exclude: bool,
) {
    if exclude {
        sql.push_str("NOT ");
    }
    sql.push_str(&format!(
        "EXISTS (SELECT 1 FROM {table} WHERE track_id = tracks.id AND LOWER({column}) = LOWER(?))"
    ));
    values.push(value.to_string());
}

#[cfg(test)]
mod tests {
    use super::{MixTag, parse_filter, parse_tag};

    #[test]
    fn parses_bare_controlled_values_and_explicit_labels() {
        assert_eq!(parse_tag(" eerie ").unwrap(), MixTag::Mood("eerie".into()));
        assert_eq!(
            parse_tag("label=Baron").unwrap(),
            MixTag::Label("baron".into())
        );
        assert_eq!(
            parse_filter(Some("eerie, texture=choral, label=night"))
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn rejects_invalid_namespaced_values() {
        assert!(parse_tag("mood=not-a-mood").is_err());
        assert!(parse_tag("bogus=eerie").is_err());
    }
}
