-- Add migration script here
CREATE TABLE tracks (
    id TEXT PRIMARY KEY,
    upload_date TEXT NOT NULL,
    yt_title TEXT NOT NULL,
    yt_channel TEXT NOT NULL,
    track_title TEXT NOT NULL,
    track_artist TEXT NOT NULL,
    track_origin TEXT NOT NULL,
    tags TEXT NOT NULL -- Store tags as a JSON array
);