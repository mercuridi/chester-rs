CREATE TABLE IF NOT EXISTS tracks (
    id TEXT PRIMARY KEY,
    upload_date TEXT NOT NULL,
    yt_title TEXT NOT NULL,
    track_title TEXT NOT NULL,
    artist_id INTEGER NOT NULL,
    origin_id INTEGER NOT NULL,
    FOREIGN KEY (artist_id) REFERENCES artists (id) ON DELETE CASCADE,
    FOREIGN KEY (origin_id) REFERENCES origins (id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS tags (
    id INTEGER PRIMARY KEY AUTOINCREMENT, -- Unique identifier for each tag
    tag TEXT NOT NULL UNIQUE              -- The tag name (must be unique)
);

CREATE TABLE IF NOT EXISTS artists (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    artist TEXT NOT NULL UNIQUE
);

CREATE TABLE IF NOT EXISTS origins (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    origin TEXT NOT NULL UNIQUE
);

CREATE TABLE IF NOT EXISTS track_tags (
    track_id TEXT NOT NULL,               -- Foreign key referencing `tracks.id`
    tag_id INTEGER NOT NULL,              -- Foreign key referencing `tags.id`
    PRIMARY KEY (track_id, tag_id),       -- Composite primary key to prevent duplicates
    FOREIGN KEY (track_id) REFERENCES tracks (id) ON DELETE CASCADE,
    FOREIGN KEY (tag_id) REFERENCES tags (id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_artists_lower ON artists(LOWER(artist));
CREATE INDEX IF NOT EXISTS idx_origins_lower ON origins(LOWER(origin));
CREATE INDEX IF NOT EXISTS idx_tags_lower ON tags(LOWER(tag));
CREATE INDEX IF NOT EXISTS idx_tracks_lower_title ON tracks(LOWER(track_title));

INSERT OR IGNORE INTO artists (artist) VALUES ("No artist provided");
INSERT OR IGNORE INTO origins (origin) VALUES ("No origin provided");
