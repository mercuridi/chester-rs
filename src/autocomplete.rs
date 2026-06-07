use crate::definitions::{PoiseContext, MetadataKind};
use crate::repository::{search_metadata, search_tracks};
use std::collections::HashSet;
use poise::serenity_prelude::AutocompleteChoice;
use crate::constants::{ELLIPSIS_LEN, ELLIPSIS};
use crate::library::lightweight_trim;

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

    let mut choices: HashSet<String> = HashSet::with_capacity(AUTOCOMPLETE_MAX_CHOICES);
    for raw in results {
        let display = lightweight_trim(raw, AUTOCOMPLETE_MAX_LENGTH);
        if needle.is_empty() || display.to_lowercase().contains(&needle) {
            choices.insert(display);
            if choices.len() >= AUTOCOMPLETE_MAX_CHOICES {
                break;
            }
        }
    }

    let mut choices: Vec<String> = choices.into_iter().collect();
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

pub fn build_autocomplete_display(mut to_display: Vec<String>) -> String {
    // Build a display name
    let content_max_length = AUTOCOMPLETE_MAX_LENGTH - (AUTOCOMPLETE_SEPARATOR_LEN * to_display.len()) + 1;

    let mut lens: Vec<usize> = to_display
        .iter()
        .map(|n| n.len())
        .collect();
    let total_len: usize = lens.iter().sum();
    let mut excess = total_len.saturating_sub(content_max_length);

    // truncate each as needed
    while excess > 0 {
        // pick the index of the longest field
        let (max_idx, &max_len) = lens
            .iter()
            .enumerate()
            .max_by_key(|&(_, &l)| l)
            .expect("lens vector should never be empty when truncating autocomplete display");

        // decide how many bytes to chop
        let chop = excess.min(max_len);
        let mut new_len = max_len.saturating_sub(chop);

        // reserve room for ellipsis if we're actually cutting
        let needs_ellipsis = new_len < max_len;
        if needs_ellipsis && new_len > ELLIPSIS_LEN {
            new_len = new_len.saturating_sub(ELLIPSIS_LEN);
        }

        // get the mutable String reference
        let s: &mut String = &mut to_display[max_idx];

        // back up to a valid UTF-8 boundary
        let mut adjust = new_len;
        while adjust > 0 && !s.is_char_boundary(adjust) {
            adjust -= 1;
        }
        s.truncate(adjust);

        // append ellipsis if we cut something
        if needs_ellipsis {
            s.push_str(ELLIPSIS);
            lens[max_idx] = adjust + ELLIPSIS_LEN;
        } else {
            lens[max_idx] = adjust;
        }

        excess = excess.saturating_sub(chop);
    }

    to_display.join(AUTOCOMPLETE_SEPARATOR)

}

