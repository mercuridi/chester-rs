pub enum MetadataKind {
    Artist,
    Origin,
}

impl MetadataKind {
    pub fn select_sql(&self) -> &'static str {
        match self {
            MetadataKind::Artist => "SELECT id FROM artists WHERE artist = ?1",
            MetadataKind::Origin => "SELECT id FROM origins WHERE origin = ?1",
        }
    }

    pub fn upsert_sql(&self) -> &'static str {
        match self {
            MetadataKind::Artist => {
                "INSERT INTO artists (artist) VALUES (?1) ON CONFLICT(artist) DO NOTHING"
            }
            MetadataKind::Origin => {
                "INSERT INTO origins (origin) VALUES (?1) ON CONFLICT(origin) DO NOTHING"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::MetadataKind;

    #[test]
    fn metadata_kinds_use_the_expected_tables_and_columns() {
        let cases = [
            (MetadataKind::Artist, "artists", "artist"),
            (MetadataKind::Origin, "origins", "origin"),
        ];

        for (kind, table, column) in cases {
            assert!(kind.select_sql().contains(table));
            assert!(kind.select_sql().contains(column));
            assert!(kind.upsert_sql().contains(table));
            assert!(kind.upsert_sql().contains(column));
            assert!(kind.select_sql().contains("?1"));
            assert!(kind.upsert_sql().contains("?1"));
        }
    }
}
