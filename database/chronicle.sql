-- Chronicle's complete derived-index schema. Bump INDEX_FORMAT_VERSION in
-- src/chronicle/indexer/db/schema.rs whenever this file changes.
CREATE TABLE IF NOT EXISTS documents (
    id           INTEGER PRIMARY KEY,
    path         TEXT NOT NULL UNIQUE,
    content_hash TEXT NOT NULL,
    metadata_hash TEXT NOT NULL DEFAULT '',
    indexed_at   TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS chunks (
    id                INTEGER PRIMARY KEY,
    document_id       INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    chunk_index       INTEGER NOT NULL,
    heading           TEXT,
    text              TEXT NOT NULL,
    visibility        TEXT NOT NULL,
    overlaps_previous INTEGER NOT NULL DEFAULT 0,
    UNIQUE (document_id, chunk_index)
);

CREATE VIRTUAL TABLE IF NOT EXISTS chunk_embeddings_player USING vec0(embedding float[384]);
CREATE VIRTUAL TABLE IF NOT EXISTS chunk_embeddings_secret USING vec0(embedding float[384]);
CREATE VIRTUAL TABLE IF NOT EXISTS chunk_fts USING fts5(heading, text, content = 'chunks', content_rowid = 'id');

CREATE TRIGGER IF NOT EXISTS chunks_fts_insert AFTER INSERT ON chunks BEGIN
    INSERT INTO chunk_fts(rowid, heading, text) VALUES (new.id, new.heading, new.text);
END;
CREATE TRIGGER IF NOT EXISTS chunks_fts_delete AFTER DELETE ON chunks BEGIN
    INSERT INTO chunk_fts(chunk_fts, rowid, heading, text) VALUES ('delete', old.id, old.heading, old.text);
END;
CREATE TRIGGER IF NOT EXISTS chunks_fts_update AFTER UPDATE ON chunks BEGIN
    INSERT INTO chunk_fts(chunk_fts, rowid, heading, text) VALUES ('delete', old.id, old.heading, old.text);
    INSERT INTO chunk_fts(rowid, heading, text) VALUES (new.id, new.heading, new.text);
END;

CREATE TABLE IF NOT EXISTS note_metadata (
    document_id INTEGER PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE,
    note_id TEXT NOT NULL, note_type TEXT NOT NULL, status TEXT NOT NULL,
    visibility TEXT NOT NULL, aliases TEXT NOT NULL, tags TEXT NOT NULL,
    summary TEXT NOT NULL, created TEXT NOT NULL, updated TEXT NOT NULL,
    author TEXT
);
CREATE TABLE IF NOT EXISTS adventure_metadata (document_id INTEGER PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE, adventure_status TEXT, start_date TEXT, end_date TEXT, system TEXT, part_of_adventure TEXT, level_range TEXT);
CREATE TABLE IF NOT EXISTS aspect_metadata (document_id INTEGER PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE);
CREATE TABLE IF NOT EXISTS character_metadata (document_id INTEGER PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE, race TEXT, life_status_cause TEXT, life_status_since TEXT, location TEXT, birthplace TEXT, birth_year TEXT, nationality TEXT, played_by TEXT, pronouns TEXT, sexuality TEXT);
CREATE TABLE IF NOT EXISTS deity_metadata (document_id INTEGER PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE, deity_type TEXT, domain TEXT, antidomain TEXT, alignment TEXT, form TEXT, crystal TEXT);
CREATE TABLE IF NOT EXISTS event_metadata (document_id INTEGER PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE, event_type TEXT, occurred TEXT, occurred_start TEXT, occurred_end TEXT, historicity TEXT, result TEXT);
CREATE TABLE IF NOT EXISTS language_metadata (document_id INTEGER PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE);
CREATE TABLE IF NOT EXISTS location_metadata (document_id INTEGER PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE, location_type TEXT, contained_in TEXT, population TEXT, demonym TEXT);
CREATE TABLE IF NOT EXISTS lore_metadata (document_id INTEGER PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE, lore_type TEXT, common_knowledge INTEGER);
CREATE TABLE IF NOT EXISTS metagame_metadata (document_id INTEGER PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE, category TEXT, system TEXT, session_date TEXT);
CREATE TABLE IF NOT EXISTS monster_metadata (document_id INTEGER PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE, creature_type TEXT, threat_level TEXT, alignment TEXT, source_inspiration TEXT);
CREATE TABLE IF NOT EXISTS object_metadata (document_id INTEGER PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE, object_type TEXT, rarity TEXT, owner TEXT, location TEXT, creator TEXT, attunement TEXT);
CREATE TABLE IF NOT EXISTS organisation_metadata (document_id INTEGER PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE, organisation_type TEXT, leader TEXT, founder TEXT, headquarters TEXT, founded TEXT, dissolved TEXT, motto TEXT);
CREATE TABLE IF NOT EXISTS race_metadata (document_id INTEGER PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE, lifespan TEXT, playable INTEGER);
CREATE TABLE IF NOT EXISTS note_wikilinks (document_id INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE, field_name TEXT NOT NULL, position INTEGER NOT NULL, value TEXT NOT NULL, PRIMARY KEY (document_id, field_name, position));
CREATE TABLE IF NOT EXISTS note_string_lists (document_id INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE, field_name TEXT NOT NULL, position INTEGER NOT NULL, value TEXT NOT NULL, PRIMARY KEY (document_id, field_name, position));
CREATE TABLE IF NOT EXISTS note_scalar_fields (document_id INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE, field_name TEXT NOT NULL, value TEXT NOT NULL, PRIMARY KEY (document_id, field_name));
CREATE TABLE IF NOT EXISTS note_identifiers (document_id INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE, value TEXT NOT NULL, PRIMARY KEY (document_id, value));
CREATE TABLE IF NOT EXISTS document_graph_edges (
    source_document_id INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    target_document_id INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    origin TEXT NOT NULL,
    field_name TEXT NOT NULL DEFAULT '',
    visibility TEXT NOT NULL,
    PRIMARY KEY (source_document_id, target_document_id, origin, field_name, visibility)
);
CREATE TABLE IF NOT EXISTS document_pagerank (
    document_id INTEGER PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE,
    player_score REAL NOT NULL,
    player_rank INTEGER NOT NULL,
    gm_score REAL NOT NULL,
    gm_rank INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS chronicle_index_state (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS note_wikilinks_lookup ON note_wikilinks(field_name, value);
CREATE INDEX IF NOT EXISTS note_string_lists_lookup ON note_string_lists(field_name, value);
CREATE INDEX IF NOT EXISTS note_scalar_fields_lookup ON note_scalar_fields(field_name, value);
CREATE INDEX IF NOT EXISTS note_identifiers_lookup ON note_identifiers(value);
CREATE INDEX IF NOT EXISTS document_graph_edges_target ON document_graph_edges(target_document_id);
