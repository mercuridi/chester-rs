CREATE TABLE IF NOT EXISTS artists (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    artist TEXT NOT NULL UNIQUE
);

CREATE TABLE IF NOT EXISTS origins (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    origin TEXT NOT NULL UNIQUE
);

-- The three playlist axes are deliberately constrained. NULL means that a
-- track has not been classified yet; once classified, each axis has one value.
CREATE TABLE IF NOT EXISTS tracks (
    id TEXT PRIMARY KEY,
    upload_date TEXT NOT NULL,
    yt_title TEXT NOT NULL,
    track_title TEXT NOT NULL,
    artist_id INTEGER NOT NULL,
    origin_id INTEGER NOT NULL,
    mood TEXT CHECK (mood IS NULL OR mood IN (
        'serene', 'warm', 'playful', 'whimsical', 'hopeful', 'wistful',
        'somber', 'mysterious', 'eerie', 'ominous', 'menacing', 'tense',
        'anxious', 'majestic', 'chaotic', 'triumphant'
    )),
    intensity TEXT CHECK (intensity IS NULL OR intensity IN (
        'subtle', 'measured', 'driving', 'fierce'
    )),
    function_tag TEXT CHECK (function_tag IS NULL OR function_tag IN (
        'exploratory', 'investigative', 'traveling', 'social', 'romantic',
        'combative', 'climactic', 'stealthy', 'ceremonial', 'celebratory',
        'contemplative', 'conversational'
    )),
    FOREIGN KEY (artist_id) REFERENCES artists (id) ON DELETE CASCADE,
    FOREIGN KEY (origin_id) REFERENCES origins (id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS track_textures (
    track_id TEXT NOT NULL,
    texture TEXT NOT NULL CHECK (texture IN (
        'ambient', 'acoustic', 'orchestral', 'electronic', 'synthetic', 'folk',
        'piano', 'percussive', 'choral', 'vocal', 'minimalist', 'ethereal',
        'dissonant'
    )),
    PRIMARY KEY (track_id, texture),
    FOREIGN KEY (track_id) REFERENCES tracks (id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS track_environments (
    track_id TEXT NOT NULL,
    environment TEXT NOT NULL CHECK (environment IN (
        'desert', 'forest', 'tundra', 'mountainous', 'coastal', 'oceanic',
        'swampy', 'urban', 'rural', 'underground', 'ruined', 'sacred',
        'otherworldly', 'celestial', 'infernal'
    )),
    PRIMARY KEY (track_id, environment),
    FOREIGN KEY (track_id) REFERENCES tracks (id) ON DELETE CASCADE
);

-- Labels are intentionally free-form and are not used as playlist axes.
CREATE TABLE IF NOT EXISTS track_labels (
    track_id TEXT NOT NULL,
    label TEXT NOT NULL COLLATE NOCASE,
    PRIMARY KEY (track_id, label),
    FOREIGN KEY (track_id) REFERENCES tracks (id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_artists_lower ON artists(LOWER(artist));
CREATE INDEX IF NOT EXISTS idx_origins_lower ON origins(LOWER(origin));
CREATE INDEX IF NOT EXISTS idx_tracks_lower_title ON tracks(LOWER(track_title));
CREATE INDEX IF NOT EXISTS idx_tracks_mood_intensity ON tracks(mood, intensity);
CREATE INDEX IF NOT EXISTS idx_track_environments_environment ON track_environments(environment);
CREATE INDEX IF NOT EXISTS idx_track_labels_lower ON track_labels(LOWER(label));

INSERT OR IGNORE INTO artists (artist) VALUES ('No artist provided');
INSERT OR IGNORE INTO origins (origin) VALUES ('No origin provided');
