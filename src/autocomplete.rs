use crate::definitions::Context;
use std::collections::HashSet;
use poise::serenity_prelude::AutocompleteChoice;
use crate::constants::{
    ELLIPSIS,
    ELLIPSIS_LEN
};
use crate::library::{lightweight_trim};

pub const AUTOCOMPLETE_MAX_CHOICES: usize = 25; // max  25
pub const AUTOCOMPLETE_MAX_LENGTH: usize = 100; // max 100
pub const AUTOCOMPLETE_SEPARATOR: &str = " | ";
pub const AUTOCOMPLETE_SEPARATOR_LEN: usize = AUTOCOMPLETE_SEPARATOR.len();

pub async fn autocomplete_artist(
    ctx: Context<'_>,
    partial: &str,
) -> impl Iterator<Item = String> {
    autocomplete_metadata(ctx, partial, "artist").await
}

pub async fn autocomplete_origin(
    ctx: Context<'_>,
    partial: &str,
) -> impl Iterator<Item = String> {
    autocomplete_metadata(ctx, partial, "origin").await
}

pub async fn autocomplete_tag(
    ctx: Context<'_>,
    partial: &str,
) -> impl Iterator<Item = String> {
    autocomplete_metadata(ctx, partial, "tag").await
}

async fn autocomplete_metadata(
    ctx: Context<'_>,
    partial: &str,
    mode: &str
) -> impl Iterator<Item = String> {
    println!("Autocomplete requested: metadata");

    let needle = partial.to_lowercase();
    let mut choices: HashSet<String> = HashSet::with_capacity(AUTOCOMPLETE_MAX_CHOICES);

    // Query the database for candidates based on the command
    let db_pool = &ctx.data().db_pool;
    let query = match mode {
        "tag" => "SELECT DISTINCT tag FROM tags WHERE LOWER(tag) LIKE ?1 LIMIT ?2",
        "artist" => "SELECT DISTINCT artist FROM artists WHERE LOWER(artist) LIKE ?1 LIMIT ?2",
        "origin" => "SELECT DISTINCT origin FROM origins WHERE LOWER(origin) LIKE ?1 LIMIT ?2",
        _ => return vec![].into_iter(), // Return an empty iterator for unsupported commands
    };

    let results: Vec<String> = sqlx::query_scalar(query)
        .bind(format!("%{}%", needle)) // Bind the search term with wildcards
        .bind(AUTOCOMPLETE_MAX_CHOICES as i64) // Bind the limit
        .fetch_all(db_pool)
        .await
        .unwrap_or_else(|err| {
            println!("Database query failed: {}", err);
            Vec::new()
        });

    // Process the results
    for raw in results {
        let display = lightweight_trim(raw, AUTOCOMPLETE_MAX_LENGTH);

        if needle.is_empty() || display.to_lowercase().contains(&needle) {
            choices.insert(display);
            if choices.len() >= AUTOCOMPLETE_MAX_CHOICES {
                break;
            }
        }
    }

    println!("Choices: {:#?}", choices.clone());
    println!("Command invoking autocomplete: {}", ctx.command().name.as_str());
    println!("Mode of autocomplete: {}", mode);
    println!("Number of choices: {}", choices.len());
    println!("Search term: {}", partial);

    let mut choices: Vec<String> = choices.into_iter().collect();
    choices.sort_unstable();
    choices.into_iter()
}

pub async fn autocomplete_track(
    ctx: Context<'_>,
    partial: &str,
) -> impl Iterator<Item = AutocompleteChoice> {
    println!("Autocomplete requested: tracks");

    let needle = partial.to_lowercase();
    let db_pool = &ctx.data().db_pool;

    // Query the database for tracks matching the partial input or associated tags
    let query = "
        SELECT DISTINCT tracks.id, tracks.track_title, artists.artist, origins.origin,
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
        LIMIT ?2
    ";

    let results: Vec<(String, String, String, String, Option<String>)> = sqlx::query_as(query)
        .bind(format!("%{}%", needle)) // Bind the search term with wildcards
        .bind(AUTOCOMPLETE_MAX_CHOICES as i64) // Bind the limit
        .fetch_all(db_pool)
        .await
        .unwrap_or_else(|err| {
            println!("Database query failed: {}", err);
            Vec::new()
        });

    // Process the results into autocomplete choices
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
            .unwrap();

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

