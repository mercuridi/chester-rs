use chrono::NaiveDateTime;
use poise::serenity_prelude::AutocompleteChoice;

use crate::chronicle::recording::recorder::RecordingManifest;
use crate::discord::constants::{AUTOCOMPLETE_MAX_CHOICES, AUTOCOMPLETE_MAX_LENGTH};
use crate::discord::context::PoiseContext;
use crate::discord::voice::require_guild;
use crate::jester::db::metadata::MetadataKind;
use crate::jester::db::repository::{search_incomplete_tracks, search_metadata, search_tracks};
use crate::jester::db::taxonomy::{ENVIRONMENTS, FUNCTIONS, INTENSITIES, MOODS, TEXTURES};
use crate::utils::format::{build_autocomplete_display, lightweight_trim};

fn autocomplete_limit() -> i64 {
    i64::try_from(AUTOCOMPLETE_MAX_CHOICES).unwrap_or(i64::MAX)
}

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

async fn autocomplete_taxonomy(
    partial: &str,
    values: &'static [&'static str],
) -> impl Iterator<Item = String> {
    let needle = partial.to_lowercase();
    values
        .iter()
        .filter(|value| value.contains(&needle))
        .map(|value| (*value).to_string())
        .collect::<Vec<_>>()
        .into_iter()
}

pub async fn autocomplete_mood(
    _ctx: PoiseContext<'_>,
    partial: &str,
) -> impl Iterator<Item = String> {
    autocomplete_taxonomy(partial, MOODS).await
}

pub async fn autocomplete_intensity(
    _ctx: PoiseContext<'_>,
    partial: &str,
) -> impl Iterator<Item = String> {
    autocomplete_taxonomy(partial, INTENSITIES).await
}

pub async fn autocomplete_function(
    _ctx: PoiseContext<'_>,
    partial: &str,
) -> impl Iterator<Item = String> {
    autocomplete_taxonomy(partial, FUNCTIONS).await
}

pub async fn autocomplete_texture(
    _ctx: PoiseContext<'_>,
    partial: &str,
) -> impl Iterator<Item = String> {
    autocomplete_taxonomy(partial, TEXTURES).await
}

pub async fn autocomplete_environment(
    _ctx: PoiseContext<'_>,
    partial: &str,
) -> impl Iterator<Item = String> {
    autocomplete_taxonomy(partial, ENVIRONMENTS).await
}

async fn autocomplete_metadata(
    ctx: PoiseContext<'_>,
    partial: &str,
    kind: MetadataKind,
) -> impl Iterator<Item = String> {
    let needle = partial.to_lowercase();
    let db_pool = &ctx.data().db_pool;

    let results = match search_metadata(db_pool, kind, &needle, autocomplete_limit()).await {
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

    let results = match search_tracks(db_pool, &needle, autocomplete_limit()).await {
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
        .collect::<Vec<_>>() // collect into Vec<AutocompleteChoice>...
        .into_iter() // ...then re-iterate, matching the early return type
}

/// Offers upcoming queue entries, returning their one-based position as value.
pub async fn autocomplete_queue_position(
    ctx: PoiseContext<'_>,
    partial: &str,
) -> impl Iterator<Item = AutocompleteChoice> {
    let guild_id = match require_guild(ctx) {
        Ok(guild_id) => guild_id,
        Err(error) => {
            tracing::error!("Failed to get guild for queue autocomplete: {error}");
            return Vec::new().into_iter();
        }
    };

    let needle = partial.to_lowercase();
    ctx.data()
        .player
        .queue_snapshot(guild_id)
        .await
        .upcoming
        .into_iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let position = index + 1;
            let display = format!(
                "{position}. {}",
                build_autocomplete_display(vec![
                    entry.track.title,
                    entry.track.artist,
                    entry.track.origin
                ])
            );
            (needle.is_empty()
                || display.to_lowercase().contains(&needle)
                || position.to_string().starts_with(&needle))
            .then(|| AutocompleteChoice::new(display, position.to_string()))
        })
        .take(AUTOCOMPLETE_MAX_CHOICES)
        .collect::<Vec<_>>()
        .into_iter()
}

pub async fn autocomplete_incomplete_track(
    ctx: PoiseContext<'_>,
    partial: &str,
) -> impl Iterator<Item = AutocompleteChoice> {
    let needle = partial.to_lowercase();
    let db_pool = &ctx.data().db_pool;

    let results = match search_incomplete_tracks(db_pool, &needle, autocomplete_limit()).await {
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

pub async fn autocomplete_existing_transcript(
    ctx: PoiseContext<'_>,
    partial: &str,
) -> impl Iterator<Item = AutocompleteChoice> {
    let guild_id = match require_guild(ctx) {
        Ok(guild_id) => guild_id,
        Err(error) => {
            tracing::error!("Failed to get guild for transcript autocomplete: {error}");
            return Vec::new().into_iter();
        }
    };

    let needle = partial.to_lowercase();

    let recording_dir = ctx
        .data()
        .config
        .paths
        .recordings_dir
        .clone()
        .join(guild_id.to_string());

    let entries = match std::fs::read_dir(&recording_dir) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::error!(
                ?error,
                path = %recording_dir.display(),
                "Failed to read recording directory for transcript autocomplete"
            );
            return Vec::new().into_iter();
        }
    };

    let mut sessions = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();

        if !path.is_dir() || !path.join("transcript.md").is_file() {
            continue;
        }

        let Some(session) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };

        let manifest = RecordingManifest::load(path.join("manifest.toml")).ok();
        let title = manifest
            .as_ref()
            .map(|manifest| manifest.session_title.as_str())
            .filter(|title| !title.is_empty());

        if !session.to_lowercase().contains(&needle)
            && !title.is_some_and(|title| title.to_lowercase().contains(&needle))
        {
            continue;
        }

        let display = format_session_display_name(session, title);

        sessions.push((session.to_owned(), display));
    }

    sessions.sort_unstable();
    sessions.truncate(AUTOCOMPLETE_MAX_CHOICES);

    sessions
        .into_iter()
        .map(|(session, display)| AutocompleteChoice::new(display, session))
        .collect::<Vec<_>>()
        .into_iter()
}

pub async fn autocomplete_recording_session(
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

    let recording_dir = ctx
        .data()
        .config
        .paths
        .recordings_dir
        .clone()
        .join(guild_id.to_string());

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

    let mut sessions: Vec<(String, String)> = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();

        if !path.is_dir() {
            continue;
        }

        let Some(session) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };

        let manifest = RecordingManifest::load(path.join("manifest.toml")).ok();
        let title = manifest
            .as_ref()
            .map(|manifest| manifest.session_title.as_str())
            .filter(|title| !title.is_empty());

        if !session.to_lowercase().contains(&needle)
            && !title.is_some_and(|title| title.to_lowercase().contains(&needle))
        {
            continue;
        }

        // Only offer sessions that have a manifest.
        if !path.join("manifest.toml").is_file() {
            continue;
        }

        let display = format_session_display_name(session, title);

        // Push the raw session path and the display to the vec
        sessions.push((session.to_owned(), display));
    }

    sessions.sort_unstable();
    sessions.truncate(AUTOCOMPLETE_MAX_CHOICES);

    sessions
        .into_iter()
        .map(|(session, display)| AutocompleteChoice::new(display, session))
        .collect::<Vec<_>>()
        .into_iter()
}

pub async fn autocomplete_alias_group(
    ctx: PoiseContext<'_>,
    partial: &str,
) -> impl Iterator<Item = AutocompleteChoice> {
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

    choices.sort_unstable_by(|(display_a, _), (display_b, _)| display_a.cmp(display_b));

    choices.truncate(AUTOCOMPLETE_MAX_CHOICES);

    choices
        .into_iter()
        .map(|(display, group_id)| AutocompleteChoice::new(display, group_id))
        .collect::<Vec<_>>()
        .into_iter()
}

fn format_session_display_name(session: &str, title: Option<&str>) -> String {
    // Construct pretty-printed displays
    // first 15 chars are always the timestamp due to consistent formatting
    // anything after that is the session name
    match session.split_once('-') {
        Some((date, rest)) if date.len() == 8 => match rest.split_once('-') {
            Some((time, name)) if time.len() == 6 => {
                match NaiveDateTime::parse_from_str(&format!("{date}-{time}"), "%Y%m%d-%H%M%S") {
                    Ok(datetime) => {
                        let display_date = datetime.format("%d %b %Y, %H:%M").to_string();
                        let display_name =
                            title.map_or_else(|| name.replace('-', " "), str::to_owned);

                        format!("{display_name} ({display_date})")
                    }
                    Err(_) => session.to_uppercase(),
                }
            }
            _ => session.to_uppercase(),
        },
        _ => session.to_uppercase(),
    }
}

#[cfg(test)]
#[allow(clippy::cast_possible_wrap)]
mod tests {
    use super::{autocomplete_limit, format_session_display_name};
    use crate::discord::constants::AUTOCOMPLETE_MAX_CHOICES;

    #[test]
    fn autocomplete_limit_matches_discord_choice_limit() {
        assert_eq!(autocomplete_limit(), AUTOCOMPLETE_MAX_CHOICES as i64);
    }

    #[test]
    fn formats_timestamped_session_slug() {
        assert_eq!(
            format_session_display_name("20240102-030405-session-name", None),
            "session name (02 Jan 2024, 03:04)"
        );
    }

    #[test]
    fn configured_title_replaces_slug_name() {
        assert_eq!(
            format_session_display_name("20240102-030405-session-name", Some("Session Title")),
            "Session Title (02 Jan 2024, 03:04)"
        );
    }

    #[test]
    fn empty_title_is_preserved_by_the_low_level_formatter() {
        assert_eq!(
            format_session_display_name("20240102-030405-session", Some("")),
            " (02 Jan 2024, 03:04)"
        );
    }

    #[test]
    fn malformed_sessions_fall_back_to_uppercase() {
        for session in [
            "session",
            "2024010-030405-name",
            "20240102-03040-name",
            "20241399-030405-name",
        ] {
            assert_eq!(
                format_session_display_name(session, None),
                session.to_uppercase()
            );
        }
    }
}
