use anyhow::{Context, Result, bail};
use serde::{Deserialize, Deserializer, de::Error as DeError};

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
    #[serde(default, deserialize_with = "deserialize_optional_role")]
    pub role: Option<crate::chronicle::query::plan::CharacterRole>,
    #[serde(default, deserialize_with = "deserialize_optional_status")]
    pub character_status: Option<crate::chronicle::query::plan::CharacterStatus>,
}

fn deserialize_optional_role<'de, D>(
    deserializer: D,
) -> Result<Option<crate::chronicle::query::plan::CharacterRole>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_optional_enum(deserializer, "role", |value| match value {
        "pc" => Some(crate::chronicle::query::plan::CharacterRole::Pc),
        "npc" => Some(crate::chronicle::query::plan::CharacterRole::Npc),
        "ex-pc" => Some(crate::chronicle::query::plan::CharacterRole::ExPc),
        _ => None,
    })
}

fn deserialize_optional_status<'de, D>(
    deserializer: D,
) -> Result<Option<crate::chronicle::query::plan::CharacterStatus>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_optional_enum(deserializer, "character_status", |value| match value {
        "alive" => Some(crate::chronicle::query::plan::CharacterStatus::Alive),
        "dead" => Some(crate::chronicle::query::plan::CharacterStatus::Dead),
        "missing" => Some(crate::chronicle::query::plan::CharacterStatus::Missing),
        "unknown" => Some(crate::chronicle::query::plan::CharacterStatus::Unknown),
        _ => None,
    })
}

fn deserialize_optional_enum<'de, D, T, F>(
    deserializer: D,
    field: &str,
    parse: F,
) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    F: FnOnce(&str) -> Option<T>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        tracing::warn!(field, "Empty Chronicle frontmatter value treated as unset");
        return Ok(None);
    }
    parse(value)
        .ok_or_else(|| D::Error::custom(format!("Invalid {field} value")))
        .map(Some)
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
    if !crate::chronicle::query::plan::NOTE_TYPES.contains(&metadata.note_type.as_str()) {
        bail!("Invalid frontmatter note type");
    }
    if metadata.note_type != "character"
        && (metadata.role.is_some() || metadata.character_status.is_some())
    {
        bail!("role and character_status are only valid on character notes");
    }
    Ok(Some((metadata, lines.collect())))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_character_properties_and_preserves_missing_as_unknown() -> Result<()> {
        let note = "---\nid: test\ntype: character\nstatus: canon\nvisibility: player\nrole: npc\ncharacter_status: alive\n---\n";
        let (metadata, _) = parse(note)?.context("note")?;
        assert_eq!(
            metadata
                .role
                .map(crate::chronicle::query::plan::CharacterRole::as_str),
            Some("npc")
        );
        assert!(parse(&note.replace("role: npc", "role: villain")).is_err());
        assert!(parse(&note.replace("type: character", "type: location")).is_err());
        assert!(
            parse(&note.replace("character_status: alive", "character_status: undead")).is_err()
        );
        let (missing, _) = parse(
            &note
                .replace("role: npc\n", "")
                .replace("character_status: alive\n", ""),
        )?
        .context("note")?;
        assert!(missing.role.is_none() && missing.character_status.is_none());
        let (empty, _) = parse(
            &note
                .replace("role: npc", "role: ''")
                .replace("character_status: alive", "character_status: '  '"),
        )?
        .context("note")?;
        assert!(empty.role.is_none() && empty.character_status.is_none());
        Ok(())
    }
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
