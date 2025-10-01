-- Add migration script here
-- Migration to add a `tags` table and update `track_tags` to reference `tag_id`

-- Create the new `tags` table
CREATE TABLE tags (
    id INTEGER PRIMARY KEY AUTOINCREMENT, -- Unique identifier for each tag
    tag TEXT NOT NULL UNIQUE              -- The tag name (must be unique)
);

-- Create the updated `track_tags` table
CREATE TABLE track_tags_new (
    track_id TEXT NOT NULL,               -- Foreign key referencing `tracks.id`
    tag_id INTEGER NOT NULL,              -- Foreign key referencing `tags.id`
    PRIMARY KEY (track_id, tag_id),       -- Composite primary key to prevent duplicates
    FOREIGN KEY (track_id) REFERENCES tracks (id) ON DELETE CASCADE,
    FOREIGN KEY (tag_id) REFERENCES tags (id) ON DELETE CASCADE
);

-- Migrate existing data from the old `track_tags` table
INSERT INTO tags (tag)
SELECT DISTINCT tag FROM track_tags;

INSERT INTO track_tags_new (track_id, tag_id)
SELECT tt.track_id, t.id
FROM track_tags tt
JOIN tags t ON tt.tag = t.tag;

-- Drop the old `track_tags` table
DROP TABLE track_tags;

-- Rename the new `track_tags` table to `track_tags`
ALTER TABLE track_tags_new RENAME TO track_tags;