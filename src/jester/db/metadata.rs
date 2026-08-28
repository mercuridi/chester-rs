pub enum MetadataKind {
    Artist,
    Origin,
    Tag,
}

impl MetadataKind {
    pub fn select_sql(&self) -> &'static str {
        match self {
            MetadataKind::Artist => "SELECT id FROM artists WHERE artist = ?1",
            MetadataKind::Origin => "SELECT id FROM origins WHERE origin = ?1",
            MetadataKind::Tag => "SELECT id FROM tags WHERE tag = ?1",
        }
    }

    pub fn insert_sql(&self) -> &'static str {
        match self {
            MetadataKind::Artist => "INSERT INTO artists (artist) VALUES (?1)",
            MetadataKind::Origin => "INSERT INTO origins (origin) VALUES (?1)",
            MetadataKind::Tag => "INSERT INTO tags (tag) VALUES (?1)",
        }
    }
}
