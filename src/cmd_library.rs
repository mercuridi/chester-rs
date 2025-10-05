use crate::definitions::{Context, Error};
use crate::library::{fmt_library_col};

// constants for library pagination
pub const LIBRARY_ROW_MAX_WIDTH:        usize =  56; // max 56
pub const MAX_RESULTS_PER_PAGE:         usize = 20;
pub const LIBRARY_SEPARATOR:            &str = " ";
pub const ROW_SEPARATOR:                &str = "-";

// CMD: library
pub const LIBRARY_COLUMN_WIDTH_TITLE:   usize = 16;
pub const LIBRARY_COLUMN_WIDTH_ARTIST:  usize = 14;
pub const LIBRARY_COLUMN_WIDTH_ORIGIN:  usize = 14;
pub const LIBRARY_COLUMN_WIDTH_TAGS:    usize = 12;

// CMD: library_title
pub const LIB_TIT_COLUMN_WIDTH_TITLE:   usize = 56;

// CMD: library_artist
pub const LIB_ART_COLUMN_WIDTH_ARTIST:  usize = 23;
pub const LIB_ART_COLUMN_WIDTH_TITLE:   usize = 30;

// CMD: library_origin
pub const LIB_ORI_COLUMN_WIDTH_ORIGIN:  usize = 23;
pub const LIB_ORI_COLUMN_WIDTH_TITLE:   usize = 30;

// CMD: library_tag
pub const LIB_TAG_COLUMN_WIDTH_TAGS:    usize = 12;
pub const LIB_TAG_COLUMN_WIDTH_TITLE:   usize = 44;

/// /library
#[poise::command(slash_command)]
pub async fn library(ctx: Context<'_>) -> Result<(), Error> {
    // Pass a default sort order, e.g., by track title
    library_sorted(ctx, "tracks.track_title").await
}

/// /library title
#[poise::command(slash_command)]
pub async fn library_title(ctx: Context<'_>) -> Result<(), Error> {
    // SQL query to fetch only track titles, sorted by title
    let query = "
        SELECT track_title
        FROM tracks
        ORDER BY track_title
    ";

    let db_pool = &ctx.data().db_pool;

    let titles: Vec<String> = sqlx::query_as::<_, (String,)>(query)
        .fetch_all(db_pool)
        .await
        .unwrap_or_else(|err| {
            println!("Database query failed: {}", err);
            Vec::new()
        })
        .into_iter()
        .map(|(title,)| {
            // Fill the column fully
            fmt_library_col(title, LIB_TIT_COLUMN_WIDTH_TITLE)
        })
        .collect();

    // Header
    let header = fmt_library_col("TITLE".to_string(), LIB_TIT_COLUMN_WIDTH_TITLE);
    let separator = ROW_SEPARATOR.repeat(LIBRARY_ROW_MAX_WIDTH);

    // Paginate
    let mut pages: Vec<String> = Vec::new();
    for chunk in titles.chunks(MAX_RESULTS_PER_PAGE) {
        let rows = chunk.join("\n");
        let body = format!("{}\n{}\n{}", header, separator, rows);
        pages.push(format!("```text\n{}\n```", body));
    }

    let page_refs: Vec<&str> = pages.iter().map(|s| s.as_str()).collect();
    poise::samples::paginate(ctx, &page_refs).await?;

    Ok(())
}

/// /library artist
#[poise::command(slash_command)]
pub async fn library_artist(ctx: Context<'_>) -> Result<(), Error> {
    // SQL query to fetch artist and title
    let query = "
        SELECT artists.artist, tracks.track_title
        FROM tracks
        LEFT JOIN artists ON tracks.artist_id = artists.id
        ORDER BY artists.artist, tracks.track_title
    ";

    let db_pool = &ctx.data().db_pool;

    let entries: Vec<String> = sqlx::query_as::<_, (String, String)>(query)
        .fetch_all(db_pool)
        .await
        .unwrap_or_else(|err| {
            println!("Database query failed: {}", err);
            Vec::new()
        })
        .into_iter()
        .map(|(artist, title)| {
            format!(
                "{}{}{}",
                fmt_library_col(artist, LIB_ART_COLUMN_WIDTH_ARTIST),
                LIBRARY_SEPARATOR,
                fmt_library_col(title, LIB_ART_COLUMN_WIDTH_TITLE),
            )
        })
        .collect();

    // Header
    let header = format!(
        "{}{}{}",
        fmt_library_col("ARTIST".to_string(), LIB_ART_COLUMN_WIDTH_ARTIST),
        LIBRARY_SEPARATOR,
        fmt_library_col("TITLE".to_string(), LIB_ART_COLUMN_WIDTH_TITLE),
    );
    let separator = ROW_SEPARATOR.repeat(LIBRARY_ROW_MAX_WIDTH);

    // Paginate
    let mut pages: Vec<String> = Vec::new();
    for chunk in entries.chunks(MAX_RESULTS_PER_PAGE) {
        let rows = chunk.join("\n");
        let body = format!("{}\n{}\n{}", header, separator, rows);
        pages.push(format!("```text\n{}\n```", body));
    }

    let page_refs: Vec<&str> = pages.iter().map(|s| s.as_str()).collect();
    poise::samples::paginate(ctx, &page_refs).await?;

    Ok(())
}


/// /library origin
#[poise::command(slash_command)]
pub async fn library_origin(ctx: Context<'_>) -> Result<(), Error> {
    // SQL query to fetch origin and title
    let query = "
        SELECT origins.origin, tracks.track_title
        FROM tracks
        LEFT JOIN origins ON tracks.origin_id = origins.id
        ORDER BY origins.origin, tracks.track_title
    ";

    let db_pool = &ctx.data().db_pool;

    let entries: Vec<String> = sqlx::query_as::<_, (String, String)>(query)
        .fetch_all(db_pool)
        .await
        .unwrap_or_else(|err| {
            println!("Database query failed: {}", err);
            Vec::new()
        })
        .into_iter()
        .map(|(origin, title)| {
            format!(
                "{}{}{}",
                fmt_library_col(origin, LIB_ORI_COLUMN_WIDTH_ORIGIN),
                LIBRARY_SEPARATOR,
                fmt_library_col(title, LIB_ORI_COLUMN_WIDTH_TITLE),
            )
        })
        .collect();

    // Header
    let header = format!(
        "{}{}{}",
        fmt_library_col("ORIGIN".to_string(), LIB_ORI_COLUMN_WIDTH_ORIGIN),
        LIBRARY_SEPARATOR,
        fmt_library_col("TITLE".to_string(), LIB_ORI_COLUMN_WIDTH_TITLE),
    );
    let separator = ROW_SEPARATOR.repeat(LIBRARY_ROW_MAX_WIDTH);

    // Paginate
    let mut pages: Vec<String> = Vec::new();
    for chunk in entries.chunks(MAX_RESULTS_PER_PAGE) {
        let rows = chunk.join("\n");
        let body = format!("{}\n{}\n{}", header, separator, rows);
        pages.push(format!("```text\n{}\n```", body));
    }

    let page_refs: Vec<&str> = pages.iter().map(|s| s.as_str()).collect();
    poise::samples::paginate(ctx, &page_refs).await?;

    Ok(())
}


/// /library tags
#[poise::command(slash_command)]
pub async fn library_tags(ctx: Context<'_>) -> Result<(), Error> {
    // SQL query to fetch tag and track title pairs
    let query = "
        SELECT tags.tag, tracks.track_title
        FROM tracks
        LEFT JOIN track_tags ON tracks.id = track_tags.track_id
        LEFT JOIN tags ON track_tags.tag_id = tags.id
        ORDER BY 
            CASE WHEN tags.tag IS NULL THEN 1 ELSE 0 END, 
            tags.tag, 
            tracks.track_title
    ";

    let db_pool = &ctx.data().db_pool;

    let entries: Vec<String> = sqlx::query_as::<_, (Option<String>, String)>(query)
        .fetch_all(db_pool)
        .await
        .unwrap_or_else(|err| {
            println!("Database query failed: {}", err);
            Vec::new()
        })
        .into_iter()
        .map(|(tag_opt, title)| {
            let tag = tag_opt.unwrap_or_else(|| "No tag".to_string());
            format!(
                "{}{}{}",
                fmt_library_col(tag, LIB_TAG_COLUMN_WIDTH_TAGS),
                LIBRARY_SEPARATOR,
                fmt_library_col(title, LIB_TAG_COLUMN_WIDTH_TITLE),
            )
        })
        .collect();

    // Header
    let header = format!(
        "{}{}{}",
        fmt_library_col("TAG".to_string(), LIB_TAG_COLUMN_WIDTH_TAGS),
        LIBRARY_SEPARATOR,
        fmt_library_col("TITLE".to_string(), LIB_TAG_COLUMN_WIDTH_TITLE),
    );
    let separator = ROW_SEPARATOR.repeat(LIBRARY_ROW_MAX_WIDTH);

    // Paginate
    let mut pages: Vec<String> = Vec::new();
    for chunk in entries.chunks(MAX_RESULTS_PER_PAGE) {
        let rows = chunk.join("\n");
        let body = format!("{}\n{}\n{}", header, separator, rows);
        pages.push(format!("```text\n{}\n```", body));
    }

    let page_refs: Vec<&str> = pages.iter().map(|s| s.as_str()).collect();
    poise::samples::paginate(ctx, &page_refs).await?;

    Ok(())
}



/// Return a paginated printout of the entire library
pub async fn library_sorted(ctx: Context<'_>, sort: &str) -> Result<(), Error> {
    let query = format!(
        "
        SELECT DISTINCT tracks.track_title, artists.artist, origins.origin, GROUP_CONCAT(tags.tag, ', ') AS tags
        FROM tracks
        LEFT JOIN track_tags ON tracks.id = track_tags.track_id
        LEFT JOIN tags ON track_tags.tag_id = tags.id
        LEFT JOIN artists ON tracks.artist_id = artists.id
        LEFT JOIN origins ON tracks.origin_id = origins.id
        GROUP BY tracks.id, tracks.track_title, artists.artist, origins.origin
        ORDER BY {}
        ",
        sort
    );

    let db_pool = &ctx.data().db_pool;

    let library: Vec<String> = sqlx::query_as(&query)
        .fetch_all(db_pool)
        .await
        .unwrap_or_else(|err| {
            println!("Database query failed: {}", err);
            Vec::new()
        })
        .into_iter()
        .map(|(title, artist, origin, tags): (String, String, String, Option<String>)| {
            let tags_display = tags.unwrap_or_else(|| "No tags".to_string());
        
            format!(
                "{}{}{}{}{}{}{}",
                fmt_library_col(title, LIBRARY_COLUMN_WIDTH_TITLE),
                LIBRARY_SEPARATOR,
                fmt_library_col(artist,LIBRARY_COLUMN_WIDTH_ARTIST ),
                LIBRARY_SEPARATOR,
                fmt_library_col(origin, LIBRARY_COLUMN_WIDTH_ORIGIN),
                LIBRARY_SEPARATOR,
                fmt_library_col(tags_display, LIBRARY_COLUMN_WIDTH_TAGS),
            )
        })
        .collect();

    let mut pages: Vec<String> = Vec::new();

    // Build the header once
    let header = format!(
        "{}{}{}{}{}{}{}",
        fmt_library_col("TITLE".to_string(), LIBRARY_COLUMN_WIDTH_TITLE),
        LIBRARY_SEPARATOR,
        fmt_library_col("ARTIST".to_string(), LIBRARY_COLUMN_WIDTH_ARTIST),
        LIBRARY_SEPARATOR,
        fmt_library_col("ORIGIN".to_string(), LIBRARY_COLUMN_WIDTH_ORIGIN),
        LIBRARY_SEPARATOR,
        fmt_library_col("TAGS".to_string(), LIBRARY_COLUMN_WIDTH_TAGS),
    );

    // Separator (56 chars wide: fill with '-')
    let separator = "-".repeat(LIBRARY_ROW_MAX_WIDTH);

    for chunk in library.chunks(MAX_RESULTS_PER_PAGE) {
        // Format the rows
        let rows = chunk.join("\n");

        // Put together: header + separator + rows
        let body = format!("{}\n{}\n{}", header, separator, rows);

        // Wrap in code block
        let formatted = format!("```text\n{}\n```", body);
        pages.push(formatted);
    }

    let page_refs: Vec<&str> = pages.iter().map(|s| s.as_str()).collect();
    poise::samples::paginate(ctx, &page_refs).await?;


    Ok(())
}