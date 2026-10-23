CREATE TABLE IF NOT EXISTS artists (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    artist TEXT NOT NULL UNIQUE
);

CREATE TABLE IF NOT EXISTS origins (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    origin TEXT NOT NULL UNIQUE
);

CREATE TABLE IF NOT EXISTS tracks (
    id TEXT PRIMARY KEY,
    upload_date TEXT NOT NULL,
    yt_title TEXT NOT NULL,
    track_title TEXT NOT NULL,
    artist_id INTEGER,
    origin_id INTEGER,
    FOREIGN KEY (artist_id) REFERENCES artists (id) ON DELETE CASCADE,
    FOREIGN KEY (origin_id) REFERENCES origins (id) ON DELETE CASCADE
);

-- Extensible shared tag vocabulary and track assignments.
CREATE TABLE IF NOT EXISTS tags (
    tag TEXT PRIMARY KEY COLLATE NOCASE
);

CREATE TABLE IF NOT EXISTS track_tags (
    track_id TEXT NOT NULL,
    tag TEXT NOT NULL COLLATE NOCASE,
    PRIMARY KEY (track_id, tag),
    FOREIGN KEY (track_id) REFERENCES tracks (id) ON DELETE CASCADE,
    FOREIGN KEY (tag) REFERENCES tags (tag) ON DELETE CASCADE
);

-- Each registered GM can choose their personal break track.
CREATE TABLE IF NOT EXISTS gm_break_tracks (
    user_id TEXT PRIMARY KEY,
    track_id TEXT NOT NULL,
    FOREIGN KEY (track_id) REFERENCES tracks (id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_artists_lower ON artists(LOWER(artist));
CREATE INDEX IF NOT EXISTS idx_origins_lower ON origins(LOWER(origin));
CREATE INDEX IF NOT EXISTS idx_tracks_lower_title ON tracks(LOWER(track_title));
CREATE INDEX IF NOT EXISTS idx_track_tags_tag ON track_tags(tag);
