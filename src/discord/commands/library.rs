use crate::discord::constants::{ELLIPSIS, MAX_RESULTS_PER_PAGE, META_MAX_CHARS, TITLE_MAX_CHARS};

use crate::discord::context::{Error, PoiseContext};
use crate::jester::db::repository::{
    fetch_library_all, fetch_library_by_artist, fetch_library_by_incomplete,
    fetch_library_by_origin, fetch_library_by_tag,
};

/// Top-level library command
#[poise::command(
    slash_command,
    subcommands("all", "artist", "origin", "tags", "incomplete")
)]
#[allow(clippy::unused_async)]
pub async fn library(_ctx: PoiseContext<'_>) -> Result<(), Error> {
    Ok(())
}

/// Shows the full library with all metadata for each track
#[poise::command(slash_command)]
async fn all(ctx: PoiseContext<'_>) -> Result<(), Error> {
    library_dynamic(ctx, "").await
}

/// Shows the library grouped by artist
#[poise::command(slash_command)]
async fn artist(ctx: PoiseContext<'_>) -> Result<(), Error> {
    library_dynamic(ctx, "artist").await
}

/// Shows the library grouped by origin
#[poise::command(slash_command)]
async fn origin(ctx: PoiseContext<'_>) -> Result<(), Error> {
    library_dynamic(ctx, "origin").await
}

/// Shows the library grouped by tags
#[poise::command(slash_command)]
async fn tags(ctx: PoiseContext<'_>) -> Result<(), Error> {
    library_dynamic(ctx, "tags").await
}

/// Shows tracks with incomplete metadata
#[poise::command(slash_command)]
async fn incomplete(ctx: PoiseContext<'_>) -> Result<(), Error> {
    library_dynamic(ctx, "incomplete").await
}

// ─── dispatcher ──────────────────────────────────────────────────────────────

async fn library_dynamic(ctx: PoiseContext<'_>, mode: &str) -> Result<(), Error> {
    let db_pool = &ctx.data().db_pool;

    let (raw_data, grouped) = match mode {
        "artist" => (fetch_library_by_artist(db_pool).await?, true),
        "origin" => (fetch_library_by_origin(db_pool).await?, true),
        "tags" => (fetch_library_by_tag(db_pool).await?, true),
        "incomplete" => (fetch_library_by_incomplete(db_pool).await?, false),
        _ => (fetch_library_all(db_pool).await?, false),
    };

    if raw_data.is_empty() {
        poise::say_reply(ctx, "No results found.").await?;
        return Ok(());
    }

    let (lines, page_mode) = if grouped {
        (format_grouped(raw_data), "grouped")
    } else {
        (format_flat(raw_data), "flat")
    };

    let pages = paginate(&lines, page_mode);
    let page_refs: Vec<&str> = pages.iter().map(String::as_str).collect();
    poise::samples::paginate(ctx, &page_refs).await?;

    Ok(())
}

// ─── helpers ────────────────────────────────────────────────────────────────

/// Truncate to at most `max` Unicode scalar values, appending "…" if cut.
fn trunc(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        s.to_string()
    } else {
        let cut = max.saturating_sub(1); // reserve one display cell for ellipsis
        chars[..cut].iter().collect::<String>() + ELLIPSIS
    }
}

/// Join non-empty, non-placeholder parts with " · ".
fn meta_line(parts: &[&str]) -> String {
    let placeholders = ["No artist provided", "No origin provided", "No tags", ""];
    parts
        .iter()
        .filter(|&&p| !placeholders.contains(&p))
        .map(|&p| trunc(p, META_MAX_CHARS))
        .collect::<Vec<_>>()
        .join(" · ")
}

// ─── format functions ────────────────────────────────────────────────────────

/// Two-line entry format used by /library all and /library incomplete.
///
/// ```
/// 1. Track Title
///    Artist · Origin · tag1, tag2
/// ```
fn format_flat(rows: Vec<Vec<String>>) -> Vec<String> {
    let num_width = rows.len().to_string().len();
    rows.into_iter()
        .enumerate()
        .map(|(i, cols)| {
            // cols: [title, artist, origin, tags?]  or  [title, artist, origin]
            let num = format!("{:>width$}.", i + 1, width = num_width);
            let title = trunc(cols.first().map_or("—", String::as_str), TITLE_MAX_CHARS);
            let meta_parts: Vec<&str> = cols
                .get(1..)
                .unwrap_or_default()
                .iter()
                .map(String::as_str)
                .collect();
            let meta = meta_line(&meta_parts);
            let indent = " ".repeat(num_width + 2 + 2); // lines up under the title plus two more spaces for visual separation
            if meta.is_empty() {
                format!("{num} {title}\n")
            } else {
                format!("{num} {title}\n{indent}{meta}\n")
            }
        })
        .collect()
}

/// Grouped format used by /library artist, /library origin, /library tags.
///
/// ```
/// ── Group Name
///  1. Track Title
///  2. Another Title
/// ```
fn format_grouped(rows: Vec<Vec<String>>) -> Vec<String> {
    // rows: [group_key, title]
    // We number tracks globally and emit a group header whenever the key changes.
    let total = rows.len();
    let num_width = total.to_string().len();

    let mut out: Vec<String> = Vec::with_capacity(total + 8);
    let mut last_key = String::new();
    let mut global_idx = 0usize;

    for cols in rows {
        let key = cols.first().map_or("—", String::as_str);
        let title = trunc(cols.get(1).map_or("—", String::as_str), TITLE_MAX_CHARS);

        if key != last_key {
            // Blank line before every group except the very first
            if !last_key.is_empty() {
                out.push(String::new());
            }
            out.push(format!("── {}", trunc(key, META_MAX_CHARS)));
            last_key = key.to_string();
        }

        global_idx += 1;
        let num = format!("{global_idx:>num_width$}.");
        out.push(format!("  {num} {title}"));
    }

    out
}

// ─── pagination ──────────────────────────────────────────────────────────────

/// Wrap rendered lines into Discord code-block pages of up to `max` *entries*.
///
/// For flat format, each entry is 2 lines; for grouped format, entries are 1
/// line each (plus group headers). We paginate by *entry count* for flat, and
/// by *line count* for grouped (since group headers don't count as entries).
fn paginate(lines: &[String], mode: &str) -> Vec<String> {
    if mode == "grouped" {
        // Split on blank separator lines to find logical page breaks.
        // We just chunk by MAX_RESULTS_PER_PAGE raw lines.
        lines
            .chunks(MAX_RESULTS_PER_PAGE)
            .map(|chunk| format!("```\n{}\n```", chunk.join("\n")))
            .collect()
    } else {
        // flat: each entry occupies exactly 2 lines (title + meta).
        // Chunk by entry pairs.
        lines
            .chunks(MAX_RESULTS_PER_PAGE)
            .map(|chunk| format!("```\n{}\n```", chunk.join("\n")))
            .collect()
    }
}

#[cfg(test)]
#[allow(clippy::range_plus_one)]
mod tests {
    use super::{format_flat, format_grouped, meta_line, paginate, trunc};
    use crate::discord::constants::{MAX_RESULTS_PER_PAGE, META_MAX_CHARS, TITLE_MAX_CHARS};

    #[test]
    fn truncates_unicode_and_reserves_room_for_ellipsis() {
        assert_eq!(trunc("ééé", 2), "é…");
        assert_eq!(trunc("éé", 2), "éé");
        assert_eq!(trunc("value", 0), "…");
    }

    #[test]
    fn metadata_line_omits_placeholders_and_empty_values() {
        assert_eq!(
            meta_line(&["Artist", "No origin provided", "", "tag"]),
            "Artist · tag"
        );
        assert!(meta_line(&["No artist provided", "No tags"]).is_empty());
    }

    #[test]
    fn metadata_line_truncates_each_component_independently() {
        let value = "x".repeat(META_MAX_CHARS + 5);
        let output = meta_line(&[&value, "tag"]);
        let first = output.split(" · ").next().unwrap_or_default();
        assert_eq!(first.chars().count(), META_MAX_CHARS);
        assert!(first.ends_with('…'));
    }

    #[test]
    fn flat_rows_include_metadata_and_stable_numbering() {
        let rows = vec![
            vec!["First".into(), "Artist".into(), "Origin".into()],
            vec![
                "Second".into(),
                "No artist provided".into(),
                "No origin provided".into(),
            ],
        ];
        let lines = format_flat(rows);
        assert_eq!(lines[0], "1. First\n     Artist · Origin\n");
        assert_eq!(lines[1], "2. Second\n");
    }

    #[test]
    fn flat_rows_supply_a_fallback_for_missing_title() {
        assert_eq!(format_flat(vec![Vec::new()]), vec!["1. —\n"]);
    }

    #[test]
    fn flat_rows_truncate_long_titles() {
        let output = format_flat(
            vec![vec!["x".repeat(TITLE_MAX_CHARS + 1)]]
                .into_iter()
                .collect(),
        );
        assert!(output[0].contains('…'));
    }

    #[test]
    fn grouped_rows_emit_headers_separators_and_global_numbers() {
        let rows = vec![
            vec!["A".into(), "One".into()],
            vec!["A".into(), "Two".into()],
            vec!["B".into(), "Three".into()],
        ];
        assert_eq!(
            format_grouped(rows),
            vec!["── A", "  1. One", "  2. Two", "", "── B", "  3. Three"]
        );
    }

    #[test]
    fn grouped_rows_supply_fallbacks_for_missing_columns() {
        assert_eq!(format_grouped(vec![Vec::new()]), vec!["── —", "  1. —"]);
    }

    #[test]
    fn pagination_wraps_code_blocks_and_chunks_at_limit() {
        let lines = (0..MAX_RESULTS_PER_PAGE + 1)
            .map(|index| index.to_string())
            .collect::<Vec<_>>();
        for mode in ["flat", "grouped"] {
            let pages = paginate(&lines, mode);
            assert_eq!(pages.len(), 2);
            assert!(
                pages
                    .iter()
                    .all(|page| page.starts_with("```\n") && page.ends_with("\n```"))
            );
        }
    }

    #[test]
    fn pagination_of_empty_input_is_empty() {
        assert!(paginate(&[], "flat").is_empty());
        assert!(paginate(&[], "grouped").is_empty());
    }
}
