use crate::definitions::{PoiseContext, Error};
use crate::db::repository::{fetch_library_all, fetch_library_by_artist, fetch_library_by_incomplete, fetch_library_by_origin, fetch_library_by_tag};

// constants for library pagination
const ROW_MAX_WIDTH:        usize = 56;
const MAX_RESULTS_PER_PAGE: usize = 20;
const LIBRARY_SEPARATOR:    &str  = " ";
const ROW_SEPARATOR:        &str  = "-";
const DUPLICATE_INDICATOR:  &str  = "";
const ELLIPSIS:             &str  = "…";
const ELLIPSIS_DISPLAY_WIDTH: usize = 1; // display width, not byte length

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

/// /library origin
#[poise::command(slash_command)]
async fn tags(ctx: PoiseContext<'_>) -> Result<(), Error> {
    library_dynamic(ctx, "tags").await
}

/// /library incomplete
#[poise::command(slash_command)]
async fn incomplete(ctx: PoiseContext<'_>) -> Result<(), Error> {
    library_dynamic(ctx, "incomplete").await
}

async fn library_dynamic(ctx: PoiseContext<'_>, mode: &str) -> Result<(), Error> {
    let db_pool = &ctx.data().db_pool;

    let (weights, headers, raw_data) = match mode {
        "artist" => (
            vec![1.0, 2.0],
            vec!["Artist", "Title"],
            fetch_library_by_artist(db_pool).await?,
        ),
        "origin" => (
            vec![1.5, 2.0],
            vec!["Origin", "Title"],
            fetch_library_by_origin(db_pool).await?,
        ),
        "tags" => (
            vec![1.0, 4.0],
            vec!["Tag", "Title"],
            fetch_library_by_tag(db_pool).await?,
        ),
        "incomplete" => (
            vec![1.0, 1.0, 1.0],
            vec!["Title", "Artist", "Origin"],
            fetch_library_by_incomplete(db_pool).await?,
        ),
        _ => (
            vec![2.0, 1.5, 1.5, 1.5],
            vec!["Title", "Artist", "Origin", "Tags"],
            fetch_library_all(db_pool).await?,
        ),
    };

    if raw_data.is_empty() {
        poise::say_reply(ctx, "No results found.").await?;
        return Ok(());
    }

    let (data_with_rownum, rownum_width) = add_row_numbers(raw_data);
    let col_widths = compute_column_widths(&weights, rownum_width);

    let mut headers_with_rownum = vec!["#"];
    headers_with_rownum.extend(headers.clone());
    let (header, formatted_rows) = format_table(
        &headers_with_rownum,
        &data_with_rownum,
        &col_widths,
        rownum_width,
    );

    let pages = paginate_table(&header, &formatted_rows, MAX_RESULTS_PER_PAGE);
    let page_refs: Vec<&str> = pages.iter().map(|s| s.as_str()).collect();
    poise::samples::paginate(ctx, &page_refs).await?;

    Ok(())
}

fn truncate_to_display_width(s: &str, max_display_width: usize) -> String {
    if max_display_width == 0 {
        return String::new();
    }

    // Count display characters, not bytes
    let char_count = s.chars().count();
    if char_count <= max_display_width {
        return s.to_string();
    }

    // Need to truncate — reserve room for ellipsis
    let truncate_at = max_display_width.saturating_sub(ELLIPSIS_DISPLAY_WIDTH);
    let truncated: String = s.chars().take(truncate_at).collect();
    format!("{}{}", truncated, ELLIPSIS)
}

fn pad_right(s: &str, display_width: usize) -> String {
    let char_count = s.chars().count();
    if char_count >= display_width {
        return s.to_string();
    }
    let padding = display_width - char_count;
    format!("{}{}", s, " ".repeat(padding))
}

fn pad_left(s: &str, display_width: usize) -> String {
    let char_count = s.chars().count();
    if char_count >= display_width {
        return s.to_string();
    }
    let padding = display_width - char_count;
    format!("{}{}", " ".repeat(padding), s)
}

fn compute_column_widths(weights: &[f64], rownum_width: usize) -> Vec<usize> {
    let num_content_cols = weights.len();
    // total separators = one between each column including rownum col
    let separator_count = num_content_cols; // rownum + N content cols = N separators
    let available = ROW_MAX_WIDTH
        .saturating_sub(rownum_width)
        .saturating_sub(separator_count);

    let total_weight: f64 = weights.iter().sum();

    let mut col_widths: Vec<usize> = weights
        .iter()
        .map(|w| {
            ((w / total_weight) * available as f64).floor() as usize
        })
        .map(|w| w.max(4))
        .collect();

    // Distribute any leftover chars left-to-right
    let used: usize = col_widths.iter().sum::<usize>() + rownum_width + separator_count;
    let mut leftover = ROW_MAX_WIDTH.saturating_sub(used);
    let mut i = 0;
    while leftover > 0 {
        col_widths[i] += 1;
        leftover -= 1;
        i = (i + 1) % col_widths.len();
    }

    tracing::debug!("col_widths (excl rownum): {:?}", col_widths);
    tracing::debug!(
        "total check: {} + {} rownum + {} seps = {}",
        col_widths.iter().sum::<usize>(),
        rownum_width,
        separator_count,
        col_widths.iter().sum::<usize>() + rownum_width + separator_count
    );

    col_widths
}

fn format_table(
    headers: &[&str],
    data: &[Vec<String>],
    col_widths: &[usize], // does NOT include rownum width — passed separately
    rownum_width: usize,
) -> (String, Vec<String>) {
    // Build header row
    // col_widths[0] corresponds to headers[1] (first content col after rownum)
    let header = {
        let rownum_cell = pad_left("#", rownum_width);
        let content_cells: String = headers[1..]
            .iter()
            .enumerate()
            .map(|(i, h)| {
                let truncated = truncate_to_display_width(h, col_widths[i]);
                pad_right(&truncated, col_widths[i])
            })
            .collect::<Vec<_>>()
            .join(LIBRARY_SEPARATOR);
        format!("{}{}{}", rownum_cell, LIBRARY_SEPARATOR, content_cells)
    };

    let mut previous_row: Vec<String> = vec![String::new(); headers.len() - 1];

    let formatted_rows = data
        .iter()
        .enumerate()
        .map(|(row_idx, row)| {
            // row[0] is the row number string e.g. "1."
            // row[1..] are the content columns
            let rownum_cell = pad_left(&row[0], rownum_width);

            let content_cells: String = row[1..]
                .iter()
                .enumerate()
                .map(|(col_idx, val)| {
                    let is_duplicate = val == &previous_row[col_idx]
                        && !previous_row[col_idx].is_empty()
                        && row_idx % MAX_RESULTS_PER_PAGE != 0;

                    let text = if is_duplicate {
                        DUPLICATE_INDICATOR.to_string()
                    } else {
                        truncate_to_display_width(val, col_widths[col_idx])
                    };

                    pad_right(&text, col_widths[col_idx])
                })
                .collect::<Vec<_>>()
                .join(LIBRARY_SEPARATOR);

            // Update previous row tracker (content cols only)
            for (col_idx, val) in row[1..].iter().enumerate() {
                previous_row[col_idx] = val.clone();
            }

            format!("{}{}{}", rownum_cell, LIBRARY_SEPARATOR, content_cells)
        })
        .collect();

    (header, formatted_rows)
}

fn add_row_numbers(data: Vec<Vec<String>>) -> (Vec<Vec<String>>, usize) {
    let total_rows = data.len();
    let rownum_width = total_rows.to_string().len() + 1; // e.g. "12." = 3
    let data_with_rownum = data
        .into_iter()
        .enumerate()
        .map(|(i, mut row)| {
            let mut new_row = vec![format!("{}.", i + 1)];
            new_row.append(&mut row);
            new_row
        })
        .collect();
    (data_with_rownum, rownum_width)
}

fn paginate_table(header: &str, rows: &[String], max_per_page: usize) -> Vec<String> {
    let separator = ROW_SEPARATOR.repeat(ROW_MAX_WIDTH);
    rows.chunks(max_per_page)
        .map(|chunk| {
            format!(
                "```ansi\n\u{001b}[0;39m{}\n{}\n{}\n```",
                header,
                separator,
                chunk.join("\n")
            )
        })
        .collect()
}