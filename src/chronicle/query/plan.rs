use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

pub const NOTE_TYPES: &[&str] = &[
    "adventure",
    "aspect",
    "character",
    "deity",
    "event",
    "language",
    "location",
    "lore",
    "metagame",
    "monster",
    "object",
    "organisation",
    "race",
    "template",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CharacterRole {
    Pc,
    Npc,
    ExPc,
}
impl CharacterRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pc => "pc",
            Self::Npc => "npc",
            Self::ExPc => "ex-pc",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CharacterStatus {
    Alive,
    Dead,
    Missing,
    Unknown,
}
impl CharacterStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Alive => "alive",
            Self::Dead => "dead",
            Self::Missing => "missing",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Filters {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<CharacterRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character_status: Option<CharacterStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Plan {
    Count {
        note_type: String,
        #[serde(default)]
        filters: Filters,
    },
    List {
        note_type: String,
        #[serde(default)]
        filters: Filters,
    },
    Search {},
    Unsupported {},
    Clarify {},
}

impl Plan {
    pub fn validate(&self) -> Result<()> {
        if let Self::Count { note_type, filters } | Self::List { note_type, filters } = self {
            ensure!(
                NOTE_TYPES.contains(&note_type.as_str()) && note_type != "template",
                "Unsupported note type"
            );
            ensure!(
                note_type == "character"
                    || (filters.role.is_none() && filters.character_status.is_none()),
                "Character filters require character notes"
            );
        }
        Ok(())
    }

    pub fn selection(&self) -> Option<(&str, &Filters)> {
        match self {
            Self::Count { note_type, filters } | Self::List { note_type, filters } => {
                Some((note_type, filters))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_unknown_fields_operators_and_invalid_combinations() {
        for input in [
            r#"{"operation":"count","note_type":"character","filters":{"location":"Northmere"}}"#,
            r#"{"operation":"count","note_type":"character","filters":{"role":"villain"}}"#,
            r#"{"operation":"search","sql":"DELETE FROM notes"}"#,
        ] {
            assert!(serde_json::from_str::<Plan>(input).is_err());
        }
        for input in [
            r#"{"operation":"count","note_type":"city"}"#,
            r#"{"operation":"count","note_type":"location","filters":{"role":"npc"}}"#,
        ] {
            assert!(serde_json::from_str::<Plan>(input).is_ok_and(|p| p.validate().is_err()));
        }
    }
}
