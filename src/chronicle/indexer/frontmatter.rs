use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Metadata {
    pub id: String,
    #[serde(rename = "type")]
    pub note_type: String,
    pub status: String,
    pub visibility: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub summary: String,
}

/// Missing frontmatter is ineligible; malformed frontmatter is an ingestion error.
pub fn parse(source: &str) -> Result<Option<(Metadata, String)>> {
    let source = source.trim_start_matches('\u{feff}');
    let mut lines = source.split_inclusive('\n');
    if lines.next().map(str::trim) != Some("---") {
        return Ok(None);
    }
    let mut yaml = String::new();
    let mut closed = false;
    for line in lines.by_ref() {
        if line.trim() == "---" {
            closed = true;
            break;
        }
        yaml.push_str(line);
    }
    if !closed {
        bail!("Unclosed YAML frontmatter");
    }
    let metadata: Metadata =
        serde_yaml::from_str(&yaml).context("Invalid Chronicle frontmatter")?;
    if metadata.id.trim().is_empty() || metadata.note_type.trim().is_empty() {
        bail!("Frontmatter id and type must be non-empty strings");
    }
    if !["canon", "draft", "deprecated", "speculative"].contains(&metadata.status.as_str()) {
        bail!("Invalid frontmatter status");
    }
    if !["player", "secret", "mixed"].contains(&metadata.visibility.as_str()) {
        bail!("Invalid frontmatter visibility");
    }
    Ok(Some((metadata, lines.collect())))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn separates_metadata_and_body() -> Result<()> {
        let (meta, body) = parse("---\r\nid: person\r\ntype: character\r\nstatus: canon\r\nvisibility: secret\r\naliases: [Someone]\r\nupdated: 2026-09-07\r\n---\r\n# Story\r\nHello")?.context("Expected parsed note")?;
        assert_eq!(meta.id, "person");
        assert_eq!(body, "# Story\r\nHello");
        assert!(!body.contains("updated"));
        assert!(parse("no frontmatter")?.is_none());
        assert!(parse("---\nid: broken").is_err());
        assert!(parse("---\nid: 123\n---").is_err());
        Ok(())
    }
}
