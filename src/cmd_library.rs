use crate::definitions::{Context, Error};
use crate::library::{lightweight_trim};

use sqlx::Row;

// constants for library pagination
pub const ROW_MAX_WIDTH:        usize =  56; // max 56
pub const MAX_RESULTS_PER_PAGE:         usize = 20;
pub const LIBRARY_SEPARATOR:            &str = " ";
pub const ROW_SEPARATOR:                &str = "-";

/// /library
#[poise::command(slash_command, subcommands("all", "artist", "origin", "tags"))]
pub async fn library(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// /library all
#[poise::command(slash_command)]
async fn all(ctx: Context<'_>) -> Result<(), Error> {
    library_dynamic(ctx, "").await
}

/// /library artist
#[poise::command(slash_command)]
async fn artist(ctx: Context<'_>) -> Result<(), Error> {
    library_dynamic(ctx, "artist").await
}


/// /library origin
#[poise::command(slash_command)]
async fn origin(ctx: Context<'_>) -> Result<(), Error> {
    library_dynamic(ctx, "origin").await
}

/// /library origin
#[poise::command(slash_command)]
async fn tags(ctx: Context<'_>) -> Result<(), Error> {
    library_dynamic(ctx, "tags").await
}

async fn library_dynamic(ctx: Context<'_>, mode: &str) -> Result<(), Error> {
    let db_pool = &ctx.data().db_pool;

    // Define column weights and headers based on mode
    let (weights, headers) = match mode {
        "artist" => (vec![1.0, 2.0], vec!["Artist", "Title"]),
        "origin" => (vec![1.0, 2.0], vec!["Origin", "Title"]),
        "tags" => (vec![2.0, 2.0], vec!["Tag", "Title"]),
        _ => (vec![2.0, 1.5, 1.5, 1.5], vec!["Title", "Artist", "Origin", "Tags"]),
    };
    

    // Fetch data
    let raw_data = fetch_library_rows(db_pool, mode).await;

    if raw_data.is_empty() {
        poise::say_reply(ctx, "No results found.").await?;
        return Ok(());
    }

    // Add row numbers
    let (data_with_rownum, rownum_width) = add_row_numbers(raw_data);

    // Compute column widths (rownum included)
    let col_widths = compute_column_widths(&weights, rownum_width);

    // Format table
    let mut headers_with_rownum = vec!["#"];
    headers_with_rownum.extend(headers.clone());
    let (header, formatted_rows) = format_table(&headers_with_rownum, &data_with_rownum, &col_widths);

    // Paginate
    let pages = paginate_table(&header, &formatted_rows, MAX_RESULTS_PER_PAGE);

    let page_refs: Vec<&str> = pages.iter().map(|s| s.as_str()).collect();
    poise::samples::paginate(ctx, &page_refs).await?;

    Ok(())
}


fn paginate_table(header: &str, rows: &[String], max_per_page: usize) -> Vec<String> {
    let separator = ROW_SEPARATOR.repeat(ROW_MAX_WIDTH);
    rows.chunks(max_per_page)
        .map(|chunk| {
            format!("```text\n{}\n{}\n{}\n```", header, separator, chunk.join("\n"))
        })
        .collect()
}


fn format_table(
    headers: &[&str],
    data: &[Vec<String>],
    col_widths: &[usize],
) -> (String, Vec<String>) {
    let header = headers
        .iter()
        .enumerate()
        .map(|(i, h)| {
            let text = if i == 0 { h.to_string() } else { lightweight_trim(h.to_string(), col_widths[i]) };
            if i == 0 {
                format!("{:>width$}", text, width = col_widths[i])
            } else {
                format!("{:<width$}", text, width = col_widths[i])
            }
        })
        .collect::<Vec<_>>()
        .join(LIBRARY_SEPARATOR); // single space separator

    let formatted_rows = data
        .iter()
        .map(|row| {
            row.iter()
                .enumerate()
                .map(|(i, val)| {
                    let text = if i == 0 { val.clone() } else { lightweight_trim(val.clone(), col_widths[i]) };
                    if i == 0 {
                        format!("{:>width$}", text, width = col_widths[i])
                    } else {
                        format!("{:<width$}", text, width = col_widths[i])
                    }
                })
                .collect::<Vec<_>>()
                .join(LIBRARY_SEPARATOR)
        })
        .collect();

    (header, formatted_rows)
}



fn compute_column_widths(weights: &[f64], rownum_width: usize) -> Vec<usize> {
    let num_columns = weights.len() + 1; // rownum + content
    let separator_space = num_columns - 1;

    let remaining_width = ROW_MAX_WIDTH - rownum_width;
    let total_weight: f64 = weights.iter().sum();

    let mut col_widths = vec![rownum_width];
    for w in weights {
        let width = ((*w / total_weight) * remaining_width as f64).floor() as usize;
        col_widths.push(width.max(4));
    }

    // Adjust for rounding to match total width exactly
    let current_total: usize = col_widths.iter().sum::<usize>() + separator_space;
    let mut extra_space = ROW_MAX_WIDTH as isize - current_total as isize;
    let mut i = 1;
    while extra_space > 0 {
        col_widths[i] += 1;
        extra_space -= 1;
        i += 1;
        if i >= col_widths.len() {
            i = 1;
        }
    }

    col_widths
}



fn add_row_numbers(data: Vec<Vec<String>>) -> (Vec<Vec<String>>, usize) {
    let total_rows = data.len();
    let rownum_width = total_rows.to_string().len() + 1; // e.g., "12."
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


async fn fetch_library_rows(
    db_pool: &sqlx::Pool<sqlx::Sqlite>,
    mode: &str,
) -> Vec<Vec<String>> {
    match mode {
        "artist" => {
            let query = "
                SELECT artists.artist, tracks.track_title
                FROM tracks
                LEFT JOIN artists ON tracks.artist_id = artists.id
                ORDER BY artists.artist
            ";
            sqlx::query(query)
                .fetch_all(db_pool)
                .await
                .unwrap_or_else(|err| {
                    println!("Database query failed: {}", err);
                    Vec::new()
                })
                .into_iter()
                .map(|row| {
                    vec![
                        row.try_get::<String, _>(0).unwrap_or_else(|_| "No artist".to_string()),
                        row.try_get::<String, _>(1).unwrap_or_else(|_| "No title".to_string()),
                    ]
                })
                .collect()
        }
        "origin" => {
            let query = "
                SELECT origins.origin, tracks.track_title
                FROM tracks
                LEFT JOIN origins ON tracks.origin_id = origins.id
                ORDER BY origins.origin
            ";
            sqlx::query(query)
                .fetch_all(db_pool)
                .await
                .unwrap_or_else(|err| {
                    println!("Database query failed: {}", err);
                    Vec::new()
                })
                .into_iter()
                .map(|row| {
                    vec![
                        row.try_get::<String, _>(0).unwrap_or_else(|_| "No origin".to_string()),
                        row.try_get::<String, _>(1).unwrap_or_else(|_| "No title".to_string()),
                    ]
                })
                .collect()
        }
        "tags" => {
            let query = "
                SELECT 
                    COALESCE(tags.tag, 'No tags') AS tag,
                    tracks.track_title
                FROM tracks
                LEFT JOIN track_tags ON tracks.id = track_tags.track_id
                LEFT JOIN tags ON track_tags.tag_id = tags.id
                ORDER BY 
                    CASE WHEN tags.tag IS NULL THEN 1 ELSE 0 END,
                    tag,
                    tracks.track_title
            ";
            sqlx::query(query)
                .fetch_all(db_pool)
                .await
                .unwrap_or_else(|err| {
                    println!("Database query failed: {}", err);
                    Vec::new()
                })
                .into_iter()
                .map(|row| {
                    vec![
                        row.try_get::<String, _>(0).unwrap_or_else(|_| "No tags".to_string()),
                        row.try_get::<String, _>(1).unwrap_or_else(|_| "No title".to_string()),
                    ]
                })
                .collect()
        }
        _ => {
            // default: show all tracks with artist, origin, tags concatenated
            let query = "
                SELECT tracks.track_title, artists.artist, origins.origin, GROUP_CONCAT(tags.tag, ', ') AS tags
                FROM tracks
                LEFT JOIN artists ON tracks.artist_id = artists.id
                LEFT JOIN origins ON tracks.origin_id = origins.id
                LEFT JOIN track_tags ON tracks.id = track_tags.track_id
                LEFT JOIN tags ON track_tags.tag_id = tags.id
                GROUP BY tracks.id
                ORDER BY tracks.track_title
            ";
            sqlx::query(query)
                .fetch_all(db_pool)
                .await
                .unwrap_or_else(|err| {
                    println!("Database query failed: {}", err);
                    Vec::new()
                })
                .into_iter()
                .map(|row| {
                    vec![
                        row.try_get::<String, _>(0).unwrap_or_else(|_| "No title".to_string()),
                        row.try_get::<String, _>(1).unwrap_or_else(|_| "No artist".to_string()),
                        row.try_get::<String, _>(2).unwrap_or_else(|_| "No origin".to_string()),
                        row.try_get::<String, _>(3).unwrap_or_else(|_| "".to_string()),
                    ]
                })
                .collect()
        }
    }
}
