use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use serenity::all::UserId;
use std::path::Path;

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
    pub fn save(&self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        let path = path.as_ref();

        let yaml = serde_yaml::to_string(&self.frontmatter)?;

        let contents = format!("---\n{}---\n\n{}", yaml, self.body.trim_end());

        // Don't overwrite a valid transcript with a partially-written file.
        let tmp_path = path.with_extension("md.tmp");

        std::fs::write(&tmp_path, contents)?;
        std::fs::rename(&tmp_path, path)?;

        Ok(())
    }

    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)?;

        let contents = contents
            .strip_prefix("---\n")
            .ok_or_else(|| anyhow::anyhow!("Transcript is missing frontmatter"))?;

        let (yaml, body) = contents
            .split_once("\n---\n")
            .ok_or_else(|| anyhow::anyhow!("Transcript has invalid frontmatter"))?;

        let frontmatter = serde_yaml::from_str(yaml)?;

        Ok(Self {
            frontmatter,
            body: body.trim_start().to_owned(),
        })
    }
}
