mod schema;

use rusqlite::{Connection, Result};
use tokio::sync::Mutex;
use std::sync::Arc;
use crate::definitions::TrackInfo;
use schema::CREATE_TRACKS_TABLE;

pub fn initialise_database() -> Result<Arc<Mutex<Connection>>> {
    let conn = Connection::open("media/db/library.sqlite3")?;
    conn.execute(CREATE_TRACKS_TABLE,[])?;
    Ok(Arc::new(Mutex::new(conn)))
}

pub fn insert_track(conn: &Connection, track: &TrackInfo) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO tracks (id, upload_date, yt_title, yt_channel, track_title, track_artist, track_origin, tags)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            track.id,
            track.upload_date,
            track.yt_title,
            track.yt_channel,
            track.track_title,
            track.track_artist,
            track.track_origin,
            serde_json::to_string(&track.tags).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
        ],
    )?;
    Ok(())
}

pub fn get_all_tracks(conn: &Connection) -> Result<Vec<TrackInfo>> {
    let mut stmt = conn.prepare("SELECT id, upload_date, yt_title, yt_channel, track_title, track_artist, track_origin, tags FROM tracks")?;
    let track_iter = stmt.query_map([], |row| {
        Ok(TrackInfo {
            id: row.get(0)?,
            upload_date: row.get(1)?,
            yt_title: row.get(2)?,
            yt_channel: row.get(3)?,
            track_title: row.get(4)?,
            track_artist: row.get(5)?,
            track_origin: row.get(6)?,
            tags: serde_json::from_str(row.get::<_, String>(7)?.as_str()).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
        })
    })?;

    let mut tracks = Vec::new();
    for track in track_iter {
        tracks.push(track?);
    }
    Ok(tracks)
}