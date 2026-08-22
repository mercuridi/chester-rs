use std::path::PathBuf;

use poise::serenity_prelude::AutocompleteChoice;

use crate::constants::{AUTOCOMPLETE_MAX_CHOICES, AUTOCOMPLETE_MAX_LENGTH};
use crate::db::metadata::MetadataKind;
use crate::db::repository::{search_incomplete_tracks, search_metadata, search_tracks};
use crate::discord::context::PoiseContext;
use crate::discord::voice::require_guild;
use crate::utils::format::{lightweight_trim, build_autocomplete_display};

pub async fn autocomplete_artist(
    ctx: PoiseContext<'_>,
    partial: &str,
) -> impl Iterator<Item = String> {
    autocomplete_metadata(ctx, partial, MetadataKind::Artist).await
}

pub async fn autocomplete_origin(
    ctx: PoiseContext<'_>,
    partial: &str,
) -> impl Iterator<Item = String> {
    autocomplete_metadata(ctx, partial, MetadataKind::Origin).await
}

pub async fn autocomplete_tag(
    ctx: PoiseContext<'_>,
    partial: &str,
) -> impl Iterator<Item = String> {
    autocomplete_metadata(ctx, partial, MetadataKind::Tag).await
}

async fn autocomplete_metadata(
    ctx: PoiseContext<'_>,
    partial: &str,
    kind: MetadataKind,
) -> impl Iterator<Item = String> {
    let needle = partial.to_lowercase();
    let db_pool = &ctx.data().db_pool;

    let results = match search_metadata(db_pool, kind, &needle, AUTOCOMPLETE_MAX_CHOICES as i64).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Autocomplete metadata query failed: {}", e);
            return vec![].into_iter();
        }
    };

    let mut choices: Vec<String> = results
        .into_iter()
        .map(|raw| lightweight_trim(raw, AUTOCOMPLETE_MAX_LENGTH))
        .collect();
    choices.sort_unstable();
    choices.into_iter()
}

pub async fn autocomplete_track(
    ctx: PoiseContext<'_>,
    partial: &str,
) -> impl Iterator<Item = AutocompleteChoice> {
    let needle = partial.to_lowercase();
    let db_pool = &ctx.data().db_pool;

    let results = match search_tracks(db_pool, &needle, AUTOCOMPLETE_MAX_CHOICES as i64).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Autocomplete track query failed: {}", e);
            return vec![].into_iter();
        }
    };

    let mut choices: Vec<(String, String)> = results
        .into_iter()
        .map(|(id, title, artist, origin, tags)| {
            let tags_display = tags.unwrap_or_else(|| "No tags".to_string());
            let display = build_autocomplete_display(vec![title, artist, origin, tags_display]);
            (display, id)
        })
        .collect();

    choices.sort_unstable_by(|(d1, _), (d2, _)| d1.cmp(d2));

    choices
        .into_iter()
        .map(|(display, video_id)| AutocompleteChoice::new(display, video_id))
        .collect::<Vec<_>>()  // collect into Vec<AutocompleteChoice>...
        .into_iter()          // ...then re-iterate, matching the early return type
}

pub async fn autocomplete_incomplete_track(
    ctx: PoiseContext<'_>,
    partial: &str,
) -> impl Iterator<Item = AutocompleteChoice> {
    let needle = partial.to_lowercase();
    let db_pool = &ctx.data().db_pool;

    let results = match search_incomplete_tracks(db_pool, &needle, AUTOCOMPLETE_MAX_CHOICES as i64).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Incomplete track autocomplete query failed: {}", e);
            return vec![].into_iter();
        }
    };

    let mut choices: Vec<(String, String)> = results
        .into_iter()
        .map(|(id, title, artist, origin, tags)| {
            let tags_display = tags.unwrap_or_else(|| "No tags".to_string());
            let display = build_autocomplete_display(vec![title, artist, origin, tags_display]);
            (display, id)
        })
        .collect();

    choices.sort_unstable_by(|(d1, _), (d2, _)| d1.cmp(d2));

    choices
        .into_iter()
        .map(|(display, video_id)| AutocompleteChoice::new(display, video_id))
        .collect::<Vec<_>>()
        .into_iter()
}

pub async fn autocomplete_transcription_session(
    ctx: PoiseContext<'_>,
    partial: &str,
) -> impl Iterator<Item = AutocompleteChoice> {

    let guild_id = match require_guild(ctx) {
        Ok(guild_id) => guild_id,
        Err(error) => {
            tracing::error!("Failed to get guild for session autocomplete: {error}");
            return Vec::new().into_iter();
        }
    };

    let needle = partial.to_lowercase();

    let recording_dir = PathBuf::from(format!(
        ".chronicle/recordings/{}",
        guild_id
    ));

    let entries = match std::fs::read_dir(&recording_dir) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::error!(
                ?error,
                path = %recording_dir.display(),
                "Failed to read recording directory for autocomplete"
            );
            return Vec::new().into_iter();
        }
    };

    let mut sessions = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();

        if !path.is_dir() {
            continue;
        }

        let Some(session) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };

        if !session.to_lowercase().contains(&needle) {
            continue;
        }

        // Only offer sessions that have a manifest.
        if !path.join("manifest.toml").is_file() {
            continue;
        }

        sessions.push(session.to_owned());
    }

    sessions.sort_unstable();
    sessions.truncate(AUTOCOMPLETE_MAX_CHOICES);

    sessions
        .into_iter()
        .map(|session| AutocompleteChoice::new(session.clone(), session))
        .collect::<Vec<_>>()
        .into_iter()
}

pub async fn autocomplete_alias_group(
    ctx: PoiseContext<'_>,
    partial: &str,
) -> impl Iterator<Item = AutocompleteChoice> {
    const MAX_CHOICES: usize = 25;

    let guild_id = match require_guild(ctx) {
        Ok(guild_id) => guild_id,
        Err(error) => {
            tracing::error!("Failed to get guild for alias-group autocomplete: {error}");
            return Vec::new().into_iter();
        }
    };

    let needle = partial.to_lowercase();
    let config = &ctx.data().config;

    let mut choices: Vec<(String, String)> = config
        .alias_groups_for_guild(guild_id)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(id, group)| {
            let display = format!("{} ({})", group.name, id);

            if display.to_lowercase().contains(&needle) {
                Some((display, id.to_owned()))
            } else {
                None
            }
        })
        .collect();

    choices.sort_unstable_by(|(display_a, _), (display_b, _)| {
        display_a.cmp(display_b)
    });

    choices.truncate(MAX_CHOICES);

    choices
        .into_iter()
        .map(|(display, group_id)| AutocompleteChoice::new(display, group_id))
        .collect::<Vec<_>>()
        .into_iter()
}