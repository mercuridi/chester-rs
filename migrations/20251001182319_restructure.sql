-- Add migration script here
-- Drop the old `tracks` table
DROP TABLE IF EXISTS tracks;

-- Create the new `tracks` table
CREATE TABLE tracks (
    id TEXT PRIMARY KEY,
    upload_date TEXT NOT NULL,
    yt_title TEXT NOT NULL,
    yt_channel TEXT NOT NULL,
    track_title TEXT NOT NULL,
    track_artist TEXT NOT NULL,
    track_origin TEXT NOT NULL
);

-- Create the `track_tags` table
CREATE TABLE track_tags (
    track_id TEXT NOT NULL,
    tag TEXT NOT NULL,
    PRIMARY KEY (track_id, tag),
    FOREIGN KEY (track_id) REFERENCES tracks (id) ON DELETE CASCADE
);

-- Create the `track_aliases` table
CREATE TABLE track_aliases (
    track_id TEXT NOT NULL,
    alias_id TEXT NOT NULL,
    PRIMARY KEY (track_id, alias_id),
    FOREIGN KEY (track_id) REFERENCES tracks (id) ON DELETE CASCADE
);