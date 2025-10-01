pub const CREATE_TRACKS_TABLE: &str = "
CREATE TABLE IF NOT EXISTS tracks (
    id TEXT PRIMARY KEY,
    upload_date TEXT NOT NULL,
    yt_title TEXT NOT NULL,
    yt_channel TEXT NOT NULL,
    track_title TEXT NOT NULL,
    track_artist TEXT NOT NULL,
    track_origin TEXT NOT NULL,
    tags TEXT NOT NULL
)";