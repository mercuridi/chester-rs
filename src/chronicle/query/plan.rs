use crate::chronicle::indexer::schema::{self, ValueType};
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConditionOperator {
    Equals,
    Contains,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Condition {
    pub field: String,
    pub operator: ConditionOperator,
    pub value: String,
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
    Synthesis {},
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
            for condition in &filters.conditions {
                validate_condition(note_type, condition)?;
            }
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

fn validate_condition(note_type: &str, condition: &Condition) -> Result<()> {
    ensure!(
        !condition.value.trim().is_empty(),
        "Query values cannot be empty"
    );
    let definition = schema::field_definition(note_type, &condition.field).ok_or_else(|| {
        anyhow::anyhow!(
            "Field `{}` is not available on {note_type} notes",
            condition.field
        )
    })?;
    match condition.operator {
        ConditionOperator::Equals => ensure!(
            !matches!(
                definition.value_type,
                ValueType::StringList | ValueType::WikilinkList
            ),
            "`equals` requires a scalar field"
        ),
        ConditionOperator::Contains => ensure!(
            matches!(
                definition.value_type,
                ValueType::StringList | ValueType::WikilinkList
            ),
            "`contains` requires a list field"
        ),
    }
    if let ValueType::FixedEnum(vocabulary) = definition.value_type {
        ensure!(
            schema::vocabulary_contains(vocabulary, &condition.value),
            "Invalid value `{}` for `{}`",
            condition.value,
            condition.field
        );
    }
    if matches!(
        definition.value_type,
        ValueType::WikilinkList | ValueType::Wikilink
    ) {
        ensure!(
            condition.value.trim().starts_with("[[") && condition.value.trim().ends_with("]]"),
            "Invalid wikilink query value"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_unknown_fields_operators_and_invalid_combinations() -> anyhow::Result<()> {
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
        let plan = serde_json::from_str::<Plan>(
            r#"{"operation":"list","note_type":"character","filters":{"conditions":[{"field":"appearances","operator":"contains","value":"[[Riftweavers]]"}]}}"#,
        )?;
        plan.validate()?;
        assert!(serde_json::from_str::<Plan>(r#"{"operation":"list","note_type":"character","filters":{"conditions":[{"field":"appearances","operator":"equals","value":"[[Riftweavers]]"}]}}"#)
            .is_ok_and(|plan| plan.validate().is_err()));
        Ok(())
    }

    #[test]
    fn accepts_synthesis_without_structured_query_fields() -> anyhow::Result<()> {
        let plan = serde_json::from_str::<Plan>(r#"{"operation":"synthesis"}"#)?;
        assert_eq!(plan, Plan::Synthesis {});
        plan.validate()?;
        assert!(plan.selection().is_none());
        assert!(
            serde_json::from_str::<Plan>(r#"{"operation":"synthesis","note_type":"event"}"#)
                .is_err()
        );
        Ok(())
    }
}
