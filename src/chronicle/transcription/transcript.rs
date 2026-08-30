use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use serenity::all::UserId;
use std::path::Path;
use tracing::{debug, info, instrument};

#[derive(Debug)]
pub struct TranscriptEntry {
    pub start: f64,
    pub end: f64,
    pub user_id: UserId,
    pub alias: String,
    pub text: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TranscriptFrontmatter {
    pub schema_version: u32,
    pub recording_date: DateTime<Local>,
    pub ended_at: DateTime<Local>,
    pub duration_seconds: f64,
    pub participants: Vec<TranscriptParticipant>,
    pub recording_count: usize,
    pub entry_count: usize,
    pub word_count: usize,
    pub character_count: usize,
    pub transcribed_at: DateTime<Local>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TranscriptParticipant {
    pub user_id: String,
    pub alias: String,
}

#[derive(Debug)]
pub struct TranscriptDocument {
    pub frontmatter: TranscriptFrontmatter,
    pub body: String,
}

impl TranscriptDocument {
    #[instrument(skip(self, path))]
    pub fn save(&self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        let path = path.as_ref();

        let yaml = serde_yaml::to_string(&self.frontmatter)?;

        let contents = format!("---\n{}---\n\n{}", yaml, self.body.trim_end());

        // Don't overwrite a valid transcript with a partially-written file.
        let tmp_path = path.with_extension("md.tmp");

        std::fs::write(&tmp_path, contents)?;
        std::fs::rename(&tmp_path, path)?;

        info!(
            entry_count = self.frontmatter.entry_count,
            "Saved transcript"
        );

        Ok(())
    }

    #[instrument(skip(path))]
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)?;

        let contents = contents
            .strip_prefix("---\n")
            .ok_or_else(|| anyhow::anyhow!("Transcript is missing frontmatter"))?;

        let (yaml, body) = contents
            .split_once("\n---\n")
            .ok_or_else(|| anyhow::anyhow!("Transcript has invalid frontmatter"))?;

        let frontmatter = serde_yaml::from_str(yaml)?;

        let document = Self {
            frontmatter,
            body: body.trim_start().to_owned(),
        };
        debug!(
            entry_count = document.frontmatter.entry_count,
            "Loaded transcript"
        );
        Ok(document)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{TranscriptDocument, TranscriptFrontmatter, TranscriptParticipant};
    use chrono::{Local, TimeZone};
    use std::fs;
    use tempfile::tempdir;

    fn document() -> anyhow::Result<TranscriptDocument> {
        let time = Local
            .with_ymd_and_hms(2024, 1, 2, 3, 4, 5)
            .single()
            .ok_or_else(|| anyhow::anyhow!("fixed local time is ambiguous"))?;
        Ok(TranscriptDocument {
            frontmatter: TranscriptFrontmatter {
                schema_version: 1,
                recording_date: time,
                ended_at: time,
                duration_seconds: 12.5,
                participants: vec![TranscriptParticipant {
                    user_id: "1".into(),
                    alias: "Alice".into(),
                }],
                recording_count: 1,
                entry_count: 2,
                word_count: 3,
                character_count: 4,
                transcribed_at: time,
            },
            body: "# Session\n\nBody\n".into(),
        })
    }

    #[test]
    fn transcript_round_trips_frontmatter_and_body() -> anyhow::Result<()> {
        let directory = tempdir()?;
        let path = directory.path().join("transcript.md");
        document()?.save(&path)?;
        let loaded = TranscriptDocument::load(&path)?;

        assert_eq!(loaded.frontmatter.schema_version, 1);
        assert_eq!(loaded.frontmatter.participants[0].alias, "Alice");
        assert_eq!(loaded.body, "# Session\n\nBody");
        assert!(!path.with_extension("md.tmp").exists());
        Ok(())
    }

    #[test]
    fn save_trims_only_trailing_body_whitespace() -> anyhow::Result<()> {
        let directory = tempdir()?;
        let path = directory.path().join("transcript.md");
        let mut value = document()?;
        value.body = "  body  \n\n".into();
        value.save(&path)?;
        let contents = fs::read_to_string(path)?;
        assert!(contents.ends_with("\n\n  body"));
        Ok(())
    }

    #[test]
    fn load_rejects_missing_or_unterminated_frontmatter() -> anyhow::Result<()> {
        let directory = tempdir()?;
        let path = directory.path().join("transcript.md");
        fs::write(&path, "plain body")?;
        assert!(
            TranscriptDocument::load(&path)
                .unwrap_err()
                .to_string()
                .contains("missing frontmatter")
        );
        fs::write(&path, "---\nschema_version: 1")?;
        assert!(
            TranscriptDocument::load(&path)
                .unwrap_err()
                .to_string()
                .contains("invalid frontmatter")
        );
        Ok(())
    }

    #[test]
    fn load_reports_invalid_yaml() -> anyhow::Result<()> {
        let directory = tempdir()?;
        let path = directory.path().join("transcript.md");
        fs::write(&path, "---\nnot: [valid\n---\nbody")?;
        assert!(TranscriptDocument::load(path).is_err());
        Ok(())
    }
}
