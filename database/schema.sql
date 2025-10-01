CREATE TABLE tracks (
    id TEXT PRIMARY KEY,
    upload_date TEXT NOT NULL,
    yt_title TEXT NOT NULL,
    yt_channel TEXT NOT NULL,
    track_title TEXT NOT NULL,
    track_artist TEXT NOT NULL,
    track_origin TEXT NOT NULL
);
CREATE TABLE track_aliases (
    track_id TEXT NOT NULL,
    alias_id TEXT NOT NULL,
    PRIMARY KEY (track_id, alias_id),
    FOREIGN KEY (track_id) REFERENCES tracks (id) ON DELETE CASCADE
);
CREATE TABLE tags (
    id INTEGER PRIMARY KEY AUTOINCREMENT, -- Unique identifier for each tag
    tag TEXT NOT NULL UNIQUE              -- The tag name (must be unique)
);
CREATE TABLE track_tags (
    track_id TEXT NOT NULL,               -- Foreign key referencing `tracks.id`
    tag_id INTEGER NOT NULL,              -- Foreign key referencing `tags.id`
    PRIMARY KEY (track_id, tag_id),       -- Composite primary key to prevent duplicates
    FOREIGN KEY (track_id) REFERENCES tracks (id) ON DELETE CASCADE,
    FOREIGN KEY (tag_id) REFERENCES tags (id) ON DELETE CASCADE
);