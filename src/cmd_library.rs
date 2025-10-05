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
    library_dynamic(ctx, "", "tracks.track_title").await
}

/// /library artist
#[poise::command(slash_command)]
async fn artist(ctx: Context<'_>) -> Result<(), Error> {
    library_dynamic(ctx, "artist", "artists.artist").await
}


/// /library origin
#[poise::command(slash_command)]
async fn origin(ctx: Context<'_>) -> Result<(), Error> {
    library_dynamic(ctx, "origin", "origins.origin").await
}

/// /library origin
#[poise::command(slash_command)]
async fn tags(ctx: Context<'_>) -> Result<(), Error> {
    library_dynamic(ctx, "tags", "tags.tag").await
}

async fn library_dynamic(ctx: Context<'_>, mode: &str, sort: &str) -> Result<(), Error> {
    let db_pool = &ctx.data().db_pool;

    // Define column weights and headers based on mode
    let (weights, headers) = match mode {
        "artist" => (vec![2.0, 1.0], vec!["Title", "Artist"]),
        "origin" => (vec![2.0, 1.0], vec!["Title", "Origin"]),
        _ => (vec![3.0, 1.5, 1.5, 1.0], vec!["Title", "Artist", "Origin", "Tags"]),
    };

    // 1️⃣ Fetch data
    let raw_data = fetch_library_rows(db_pool, mode, sort).await;

    if raw_data.is_empty() {
        poise::say_reply(ctx, "No results found.").await?;
        return Ok(());
    }

    // 2️⃣ Add row numbers
    let (data_with_rownum, rownum_width) = add_row_numbers(raw_data);

    // 3️⃣ Compute column widths (rownum included)
    let col_widths = compute_column_widths(&weights, rownum_width);

    // 4️⃣ Format table
    let mut headers_with_rownum = vec!["#"];
    headers_with_rownum.extend(headers.clone());
    let (header, formatted_rows) = format_table(&headers_with_rownum, &data_with_rownum, &col_widths);

    // 5️⃣ Paginate
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
        if i == 0 {
            h.to_string()
        } else {
            lightweight_trim(h.to_string(), col_widths[i])
        }
    })
    .collect::<Vec<_>>()
    .join(LIBRARY_SEPARATOR);

let formatted_rows = data
    .iter()
    .map(|row| {
        row.iter()
            .enumerate()
            .map(|(i, val)| {
                if i == 0 {
                    val.clone()
                } else {
                    lightweight_trim(val.clone(), col_widths[i])
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
    sort: &str,
) -> Vec<Vec<String>> {
    let (select_fields, num_columns) = match mode {
        "artist" => ("tracks.track_title, artists.artist", 2),
        "origin" => ("tracks.track_title, origins.origin", 2),
        _ => (
            "tracks.track_title, artists.artist, origins.origin, GROUP_CONCAT(tags.tag, ', ') AS tags",
            4,
        ),
    };

    let query = format!(
        "
        SELECT DISTINCT {select_fields}
        FROM tracks
        LEFT JOIN track_tags ON tracks.id = track_tags.track_id
        LEFT JOIN tags ON track_tags.tag_id = tags.id
        LEFT JOIN artists ON tracks.artist_id = artists.id
        LEFT JOIN origins ON tracks.origin_id = origins.id
        GROUP BY tracks.id
        ORDER BY {sort}
        "
    );

    sqlx::query(&query)
        .fetch_all(db_pool)
        .await
        .unwrap_or_else(|err| {
            println!("Database query failed: {}", err);
            Vec::new()
        })
        .into_iter()
        .map(|row| {
            (0..num_columns)
                .map(|i| row.try_get(i).unwrap_or_else(|_| Some("No data".to_string())).unwrap_or_else(|| "No data".to_string()))
                .collect::<Vec<String>>()
        })
        .collect()
}
