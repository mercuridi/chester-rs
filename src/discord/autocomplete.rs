use crate::definitions::{PoiseContext, MetadataKind};
use crate::db::repository::{search_incomplete_tracks, search_metadata, search_tracks};
use std::collections::HashSet;
use poise::serenity_prelude::AutocompleteChoice;
use crate::utils::format::{lightweight_trim, build_autocomplete_display};

pub const AUTOCOMPLETE_MAX_CHOICES: usize = 25;
pub const AUTOCOMPLETE_MAX_LENGTH: usize = 100;
pub const AUTOCOMPLETE_SEPARATOR: &str = " | ";
pub const AUTOCOMPLETE_SEPARATOR_LEN: usize = AUTOCOMPLETE_SEPARATOR.len();

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