#!/usr/bin/env bash
# migrate_json_to_sqlite.sh
# Usage: ./migrate_json_to_sqlite.sh database.sqlite3 /path/to/json/files/*.json

set -euo pipefail

DB="$1"
shift
FILES=("$@")

# Ensure DB schema exists
sqlite3 "$DB" <<'EOF'
PRAGMA foreign_keys = ON;

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
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    tag TEXT NOT NULL UNIQUE
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
    track_id TEXT NOT NULL,
    tag_id INTEGER NOT NULL,
    PRIMARY KEY (track_id, tag_id),
    FOREIGN KEY (track_id) REFERENCES tracks (id) ON DELETE CASCADE,
    FOREIGN KEY (tag_id) REFERENCES tags (id) ON DELETE CASCADE
);

-- Insert default values if not already there
INSERT OR IGNORE INTO artists (artist) VALUES ('No artist provided');
INSERT OR IGNORE INTO origins (origin) VALUES ('No origin provided');
EOF

for f in "${FILES[@]}"; do
    echo "Processing $f"

    # Extract JSON fields
    id=$(jq -r '.id' "$f")
    upload_date=$(jq -r '.upload_date' "$f")
    yt_title=$(jq -r '.yt_title' "$f")
    track_title=$(jq -r '.track_title' "$f")
    track_artist=$(jq -r '.track_artist // "No artist provided"' "$f")
    track_origin=$(jq -r '.track_origin // "No origin provided"' "$f")
    tags=$(jq -r '.tags[]?' "$f")

    esc_id=$(echo "$id" | sed "s/'/''/g")
    esc_upload_date=$(echo "$upload_date" | sed "s/'/''/g")
    esc_yt_title=$(echo "$yt_title" | sed "s/'/''/g")
    esc_track_title=$(echo "$track_title" | sed "s/'/''/g")
    esc_artist=$(echo "$track_artist" | sed "s/'/''/g")
    esc_origin=$(echo "$track_origin" | sed "s/'/''/g")

    # Build SQL for this file in one transaction
    sql="BEGIN TRANSACTION;
INSERT OR IGNORE INTO artists (artist) VALUES ('$esc_artist');
INSERT OR IGNORE INTO origins (origin) VALUES ('$esc_origin');

INSERT OR IGNORE INTO tracks (id, upload_date, yt_title, track_title, artist_id, origin_id)
VALUES (
  '$esc_id',
  '$esc_upload_date',
  '$esc_yt_title',
  '$esc_track_title',
  (SELECT id FROM artists WHERE artist = '$esc_artist'),
  (SELECT id FROM origins WHERE origin = '$esc_origin')
);
"

    # Add tag inserts
    for tag in $tags; do
        esc_tag=$(echo "$tag" | sed "s/'/''/g")
        sql+="
INSERT OR IGNORE INTO tags (tag) VALUES ('$esc_tag');
INSERT OR IGNORE INTO track_tags (track_id, tag_id)
    SELECT '$esc_id', id FROM tags WHERE tag = '$esc_tag';
"
    done

    sql+="COMMIT;"

    # Execute all in one sqlite3 call
    echo "$sql" | sqlite3 "$DB"
done

echo "Migration complete"
