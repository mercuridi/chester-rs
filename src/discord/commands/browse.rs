use crate::definitions::{PoiseContext, Error};
use crate::db::repository::{
    fetch_library_all, fetch_library_by_artist, fetch_library_by_incomplete,
    fetch_library_by_origin, fetch_library_by_tag,
};

const MAX_RESULTS_PER_PAGE: usize = 15;
const TITLE_MAX_CHARS: usize = 36;
const META_MAX_CHARS: usize = 40;
const ELLIPSIS: &str = "…";

/// /library
#[poise::command(slash_command, subcommands("all", "artist", "origin", "tags", "incomplete"))]
pub async fn library(_ctx: PoiseContext<'_>) -> Result<(), Error> {
    Ok(())
}

/// /library all
#[poise::command(slash_command)]
async fn all(ctx: PoiseContext<'_>) -> Result<(), Error> {
    library_dynamic(ctx, "").await
}

/// /library artist
#[poise::command(slash_command)]
async fn artist(ctx: PoiseContext<'_>) -> Result<(), Error> {
    library_dynamic(ctx, "artist").await
}

/// /library origin
#[poise::command(slash_command)]
async fn origin(ctx: PoiseContext<'_>) -> Result<(), Error> {
    library_dynamic(ctx, "origin").await
}

/// /library tags
#[poise::command(slash_command)]
async fn tags(ctx: PoiseContext<'_>) -> Result<(), Error> {
    library_dynamic(ctx, "tags").await
}

/// /library incomplete
#[poise::command(slash_command)]
async fn incomplete(ctx: PoiseContext<'_>) -> Result<(), Error> {
    library_dynamic(ctx, "incomplete").await
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
            let title = trunc(cols.get(0).map(String::as_str).unwrap_or("—"), TITLE_MAX_CHARS);
            let meta_parts: Vec<&str> = cols[1..].iter().map(String::as_str).collect();
            let meta = meta_line(&meta_parts);
            let indent = " ".repeat(num_width + 2 + 2); // lines up under the title plus two more spaces for visual separation
            if meta.is_empty() {
                format!("{} {}\n", num, title)
            } else {
                format!("{} {}\n{}{}\n", num, title, indent, meta)
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
        let key = cols.get(0).map(String::as_str).unwrap_or("—");
        let title = trunc(cols.get(1).map(String::as_str).unwrap_or("—"), TITLE_MAX_CHARS);

        if key != last_key {
            // Blank line before every group except the very first
            if !last_key.is_empty() {
                out.push(String::new());
            }
            out.push(format!("── {}", trunc(key, META_MAX_CHARS)));
            last_key = key.to_string();
        }

        global_idx += 1;
        let num = format!("{:>width$}.", global_idx, width = num_width);
        out.push(format!("  {} {}", num, title));
    }

    out
}

// ─── pagination ──────────────────────────────────────────────────────────────

/// Wrap rendered lines into Discord code-block pages of up to `max` *entries*.
///
/// For flat format, each entry is 2 lines; for grouped format, entries are 1
/// line each (plus group headers). We paginate by *entry count* for flat, and
/// by *line count* for grouped (since group headers don't count as entries).
fn paginate(lines: Vec<String>, mode: &str) -> Vec<String> {
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

// ─── dispatcher ──────────────────────────────────────────────────────────────

async fn library_dynamic(ctx: PoiseContext<'_>, mode: &str) -> Result<(), Error> {
    let db_pool = &ctx.data().db_pool;

    let (raw_data, grouped) = match mode {
        "artist"     => (fetch_library_by_artist(db_pool).await?,   true),
        "origin"     => (fetch_library_by_origin(db_pool).await?,   true),
        "tags"       => (fetch_library_by_tag(db_pool).await?,      true),
        "incomplete" => (fetch_library_by_incomplete(db_pool).await?, false),
        _            => (fetch_library_all(db_pool).await?,          false),
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

    let pages = paginate(lines, page_mode);
    let page_refs: Vec<&str> = pages.iter().map(String::as_str).collect();
    poise::samples::paginate(ctx, &page_refs).await?;

    Ok(())
}