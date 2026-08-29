CREATE TABLE metadata (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE documents (
    id           INTEGER PRIMARY KEY,
    path         TEXT NOT NULL UNIQUE,
    content_hash TEXT NOT NULL,
    indexed_at   TEXT NOT NULL
);

CREATE TABLE chunks (
    id            INTEGER PRIMARY KEY,
    document_id   INTEGER NOT NULL,
    chunk_index   INTEGER NOT NULL,
    heading       TEXT,
    text          TEXT NOT NULL,

    FOREIGN KEY (document_id)
        REFERENCES documents(id)
        ON DELETE CASCADE,

    UNIQUE (document_id, chunk_index)
);

CREATE VIRTUAL TABLE chunk_embeddings USING vec0(
    embedding float[384]
);