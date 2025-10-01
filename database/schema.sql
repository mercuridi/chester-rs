DROP TABLE IF EXISTS tags;
DROP TABLE IF EXISTS track_tags;
DROP TABLE IF EXISTS artists;
DROP TABLE IF EXISTS origins;
DROP TABLE IF EXISTS tracks;


CREATE TABLE tracks (
    id TEXT PRIMARY KEY,
    upload_date TEXT NOT NULL,
    yt_title TEXT NOT NULL,
    track_title TEXT NOT NULL,
    artist_id INTEGER NOT NULL,
    origin_id INTEGER NOT NULL,
    FOREIGN KEY (artist_id) REFERENCES artists (id) ON DELETE CASCADE,
    FOREIGN KEY (origin_id) REFERENCES origins (id) ON DELETE CASCADE
);

CREATE TABLE tags (
    id INTEGER PRIMARY KEY AUTOINCREMENT, -- Unique identifier for each tag
    tag TEXT NOT NULL UNIQUE              -- The tag name (must be unique)
);

CREATE TABLE artists (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    artist TEXT NOT NULL UNIQUE
);

CREATE TABLE origins (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    origin TEXT NOT NULL UNIQUE
);

CREATE TABLE track_tags (
    track_id TEXT NOT NULL,               -- Foreign key referencing `tracks.id`
    tag_id INTEGER NOT NULL,              -- Foreign key referencing `tags.id`
    PRIMARY KEY (track_id, tag_id),       -- Composite primary key to prevent duplicates
    FOREIGN KEY (track_id) REFERENCES tracks (id) ON DELETE CASCADE,
    FOREIGN KEY (tag_id) REFERENCES tags (id) ON DELETE CASCADE
);

INSERT INTO artists (artist) VALUES ("No artist provided");
INSERT INTO origins (origin) VALUES ("No origin provided");
