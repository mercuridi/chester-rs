CREATE TABLE chronicle_sessions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,

    session_uuid TEXT NOT NULL UNIQUE,

    guild_id TEXT NOT NULL,
    voice_channel_id TEXT NOT NULL,

    started_at TEXT NOT NULL,
    ended_at TEXT,

    status TEXT NOT NULL,

    storage_path TEXT NOT NULL,

    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE chronicle_speakers (
    id INTEGER PRIMARY KEY AUTOINCREMENT,

    session_id INTEGER NOT NULL,

    discord_user_id TEXT NOT NULL,
    display_name TEXT NOT NULL,

    FOREIGN KEY (session_id)
        REFERENCES chronicle_sessions(id)
        ON DELETE CASCADE,

    UNIQUE (session_id, discord_user_id)
);

CREATE INDEX idx_chronicle_sessions_guild
    ON chronicle_sessions(guild_id);

CREATE INDEX idx_chronicle_speakers_session
    ON chronicle_speakers(session_id);