//! Native runtime schema for Chronicle frontmatter.
//!
//! This module is intentionally separate from the migration specification in
//! `tmp/taxonomy.toml`. The migration specification describes legacy data
//! conversion; this module describes the fields and values accepted by the
//! running application.

/// How a field is represented in frontmatter and in the normalized metadata
/// model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueType {
    String,
    StringList,
    Boolean,
    Date,
    FantasyDate,
    Wikilink,
    WikilinkList,
    StringOrWikilink,
    FixedEnum(&'static Vocabulary),
    ExtensibleVocabulary,
}

/// Whether a field is required, optional, or receives a default when omitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    Required,
    Optional,
    DefaultEmptyList,
    DefaultEmptyStringWithWarning,
}

/// A named vocabulary used by one or more fixed-enum fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vocabulary {
    pub name: &'static str,
    pub values: &'static [&'static str],
}

/// Declarative definition of one frontmatter field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldDefinition {
    pub name: &'static str,
    pub value_type: ValueType,
    pub presence: Presence,
}

impl FieldDefinition {
    pub const fn required(name: &'static str, value_type: ValueType) -> Self {
        Self {
            name,
            value_type,
            presence: Presence::Required,
        }
    }

    pub const fn optional(name: &'static str, value_type: ValueType) -> Self {
        Self {
            name,
            value_type,
            presence: Presence::Optional,
        }
    }

    pub const fn default_empty_list(name: &'static str) -> Self {
        Self {
            name,
            value_type: ValueType::StringList,
            presence: Presence::DefaultEmptyList,
        }
    }

    pub const fn default_empty_string_with_warning(name: &'static str) -> Self {
        Self {
            name,
            value_type: ValueType::String,
            presence: Presence::DefaultEmptyStringWithWarning,
        }
    }
}

/// Fields allowed for one document type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocumentTypeDefinition {
    pub name: &'static str,
    pub fields: &'static [FieldDefinition],
}

const DOCUMENT_TYPES: &[&str] = &[
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

const STATUSES: &[&str] = &["canon", "draft", "deprecated", "speculative"];
const VISIBILITIES: &[&str] = &["player", "secret", "mixed"];
const ADVENTURE_STATUSES: &[&str] = &["completed", "planned", "ongoing"];
const SYSTEMS: &[&str] = &["5e", "draw-steel"];
const CHARACTER_ROLES: &[&str] = &["pc", "npc", "ex-pc"];
const LIFE_STATUSES: &[&str] = &["alive", "dead", "missing", "unknown"];
const DEITY_TYPES: &[&str] = &["Minor", "Major", "Forsaken"];
const METAGAME_CATEGORIES: &[&str] = &[
    "house-rule",
    "mechanic",
    "worldbuilding-note",
    "session-note",
];
const HISTORICITIES: &[&str] = &["historical", "legend", "disputed", "prophecy"];

pub const DOCUMENT_TYPES_VOCABULARY: Vocabulary = Vocabulary {
    name: "document_types",
    values: DOCUMENT_TYPES,
};
pub const STATUS: Vocabulary = Vocabulary {
    name: "status",
    values: STATUSES,
};
pub const VISIBILITY: Vocabulary = Vocabulary {
    name: "visibility",
    values: VISIBILITIES,
};
pub const ADVENTURE_STATUS: Vocabulary = Vocabulary {
    name: "adventure_status",
    values: ADVENTURE_STATUSES,
};
pub const SYSTEM: Vocabulary = Vocabulary {
    name: "system",
    values: SYSTEMS,
};
pub const CHARACTER_ROLE: Vocabulary = Vocabulary {
    name: "character_role",
    values: CHARACTER_ROLES,
};
pub const LIFE_STATUS: Vocabulary = Vocabulary {
    name: "life_status",
    values: LIFE_STATUSES,
};
pub const DEITY_TYPE: Vocabulary = Vocabulary {
    name: "deity_type",
    values: DEITY_TYPES,
};
pub const METAGAME_CATEGORY: Vocabulary = Vocabulary {
    name: "metagame_category",
    values: METAGAME_CATEGORIES,
};
pub const HISTORICITY: Vocabulary = Vocabulary {
    name: "historicity",
    values: HISTORICITIES,
};

const UNIVERSAL_FIELDS: &[FieldDefinition] = &[
    FieldDefinition::required("id", ValueType::String),
    FieldDefinition::required("type", ValueType::FixedEnum(&DOCUMENT_TYPES_VOCABULARY)),
    FieldDefinition::default_empty_list("aliases"),
    FieldDefinition::default_empty_list("tags"),
    FieldDefinition::default_empty_string_with_warning("summary"),
    FieldDefinition::required("status", ValueType::FixedEnum(&STATUS)),
    FieldDefinition::required("visibility", ValueType::FixedEnum(&VISIBILITY)),
    FieldDefinition::required("created", ValueType::Date),
    FieldDefinition::required("updated", ValueType::Date),
];

const ADVENTURE_FIELDS: &[FieldDefinition] = &[
    FieldDefinition::optional("adventure_status", ValueType::FixedEnum(&ADVENTURE_STATUS)),
    FieldDefinition::optional("start_date", ValueType::Date),
    FieldDefinition::optional("end_date", ValueType::Date),
    FieldDefinition::optional("party", ValueType::WikilinkList),
    FieldDefinition::optional("regions", ValueType::WikilinkList),
    FieldDefinition::optional("related_events", ValueType::WikilinkList),
    FieldDefinition::optional("system", ValueType::FixedEnum(&SYSTEM)),
    FieldDefinition::optional("part_of_adventure", ValueType::Wikilink),
    FieldDefinition::optional("level_range", ValueType::String),
    FieldDefinition::optional("antagonists", ValueType::WikilinkList),
];

const ASPECT_FIELDS: &[FieldDefinition] = &[
    FieldDefinition::optional("ruling_deities", ValueType::WikilinkList),
    FieldDefinition::optional("native_races", ValueType::WikilinkList),
];

const CHARACTER_FIELDS: &[FieldDefinition] = &[
    FieldDefinition::optional("race", ValueType::Wikilink),
    FieldDefinition::optional("role", ValueType::FixedEnum(&CHARACTER_ROLE)),
    FieldDefinition::optional("life_status", ValueType::FixedEnum(&LIFE_STATUS)),
    FieldDefinition::optional("life_status_cause", ValueType::StringOrWikilink),
    FieldDefinition::optional("life_status_since", ValueType::String),
    FieldDefinition::optional("appearances", ValueType::WikilinkList),
    FieldDefinition::optional("affiliations", ValueType::WikilinkList),
    FieldDefinition::optional("allies", ValueType::WikilinkList),
    FieldDefinition::optional("enemies", ValueType::WikilinkList),
    FieldDefinition::optional("parents", ValueType::WikilinkList),
    FieldDefinition::optional("siblings", ValueType::WikilinkList),
    FieldDefinition::optional("children", ValueType::WikilinkList),
    FieldDefinition::optional("partners", ValueType::WikilinkList),
    FieldDefinition::optional("other_family", ValueType::WikilinkList),
    FieldDefinition::optional("location", ValueType::Wikilink),
    FieldDefinition::optional("patron_deities", ValueType::WikilinkList),
    FieldDefinition::optional("birthplace", ValueType::Wikilink),
    FieldDefinition::optional("birth_year", ValueType::String),
    FieldDefinition::optional("nationality", ValueType::String),
    FieldDefinition::optional("played_by", ValueType::String),
    FieldDefinition::optional("pronouns", ValueType::String),
];

const DEITY_FIELDS: &[FieldDefinition] = &[
    FieldDefinition::optional("deity_type", ValueType::FixedEnum(&DEITY_TYPE)),
    FieldDefinition::optional("domain", ValueType::String),
    FieldDefinition::optional("antidomain", ValueType::String),
    FieldDefinition::optional("alignment", ValueType::String),
    FieldDefinition::optional("form", ValueType::String),
    FieldDefinition::optional("crystal", ValueType::String),
    FieldDefinition::optional("rival_deities", ValueType::WikilinkList),
    FieldDefinition::optional("worshippers", ValueType::WikilinkList),
    FieldDefinition::optional("holy_sites", ValueType::WikilinkList),
    FieldDefinition::optional("associated_aspects", ValueType::WikilinkList),
];

const EVENT_FIELDS: &[FieldDefinition] = &[
    FieldDefinition::optional("event_type", ValueType::ExtensibleVocabulary),
    FieldDefinition::optional("occurred", ValueType::FantasyDate),
    FieldDefinition::optional("occurred_start", ValueType::FantasyDate),
    FieldDefinition::optional("occurred_end", ValueType::FantasyDate),
    FieldDefinition::optional("locations", ValueType::WikilinkList),
    FieldDefinition::optional("participants", ValueType::WikilinkList),
    FieldDefinition::optional("causes", ValueType::WikilinkList),
    FieldDefinition::optional("consequences", ValueType::WikilinkList),
    FieldDefinition::optional("historicity", ValueType::FixedEnum(&HISTORICITY)),
    FieldDefinition::optional("affected_regions", ValueType::WikilinkList),
    FieldDefinition::optional("result", ValueType::String),
];

const LANGUAGE_FIELDS: &[FieldDefinition] = &[
    FieldDefinition::optional("speakers", ValueType::WikilinkList),
    FieldDefinition::optional("scripts", ValueType::StringList),
];

const LOCATION_FIELDS: &[FieldDefinition] = &[
    FieldDefinition::optional("location_type", ValueType::ExtensibleVocabulary),
    FieldDefinition::optional("contained_in", ValueType::Wikilink),
    FieldDefinition::optional("political_affiliations", ValueType::WikilinkList),
    FieldDefinition::optional("population", ValueType::String),
    FieldDefinition::optional("demonym", ValueType::String),
];

const LORE_FIELDS: &[FieldDefinition] = &[
    FieldDefinition::optional("lore_type", ValueType::ExtensibleVocabulary),
    FieldDefinition::optional("common_knowledge", ValueType::Boolean),
];

const METAGAME_FIELDS: &[FieldDefinition] = &[
    FieldDefinition::optional("category", ValueType::FixedEnum(&METAGAME_CATEGORY)),
    FieldDefinition::optional("system", ValueType::FixedEnum(&SYSTEM)),
    FieldDefinition::optional("session_date", ValueType::Date),
];

const MONSTER_FIELDS: &[FieldDefinition] = &[
    FieldDefinition::optional("creature_type", ValueType::ExtensibleVocabulary),
    FieldDefinition::optional("habitat", ValueType::WikilinkList),
    FieldDefinition::optional("threat_level", ValueType::String),
    FieldDefinition::optional("alignment", ValueType::String),
    FieldDefinition::optional("factions", ValueType::WikilinkList),
    FieldDefinition::optional("weaknesses", ValueType::StringList),
    FieldDefinition::optional("sizes", ValueType::StringList),
    FieldDefinition::optional("source_inspiration", ValueType::String),
    FieldDefinition::optional("notable_examples", ValueType::WikilinkList),
];

const OBJECT_FIELDS: &[FieldDefinition] = &[
    FieldDefinition::optional("object_type", ValueType::ExtensibleVocabulary),
    FieldDefinition::optional("rarity", ValueType::String),
    FieldDefinition::optional("owner", ValueType::Wikilink),
    FieldDefinition::optional("location", ValueType::Wikilink),
    FieldDefinition::optional("creator", ValueType::Wikilink),
    FieldDefinition::optional("attunement", ValueType::String),
];

const ORGANISATION_FIELDS: &[FieldDefinition] = &[
    FieldDefinition::optional("organisation_type", ValueType::ExtensibleVocabulary),
    FieldDefinition::optional("leader", ValueType::Wikilink),
    FieldDefinition::optional("founder", ValueType::Wikilink),
    FieldDefinition::optional("members", ValueType::WikilinkList),
    FieldDefinition::optional("allies", ValueType::WikilinkList),
    FieldDefinition::optional("enemies", ValueType::WikilinkList),
    FieldDefinition::optional("headquarters", ValueType::Wikilink),
    FieldDefinition::optional("founded", ValueType::FantasyDate),
    FieldDefinition::optional("patron_deities", ValueType::WikilinkList),
    FieldDefinition::optional("dissolved", ValueType::FantasyDate),
    FieldDefinition::optional("jurisdiction", ValueType::WikilinkList),
    FieldDefinition::optional("ideology", ValueType::StringList),
    FieldDefinition::optional("motto", ValueType::String),
];

const RACE_FIELDS: &[FieldDefinition] = &[
    FieldDefinition::optional("homeland", ValueType::WikilinkList),
    FieldDefinition::optional("lifespan", ValueType::String),
    FieldDefinition::optional("playable", ValueType::Boolean),
    FieldDefinition::optional("related_organisations", ValueType::WikilinkList),
    FieldDefinition::optional("languages", ValueType::WikilinkList),
    FieldDefinition::optional("subraces", ValueType::WikilinkList),
    FieldDefinition::optional("sizes", ValueType::StringList),
];

const TEMPLATE_FIELDS: &[FieldDefinition] = &[];

pub const DOCUMENT_TYPE_DEFINITIONS: &[DocumentTypeDefinition] = &[
    DocumentTypeDefinition {
        name: "adventure",
        fields: ADVENTURE_FIELDS,
    },
    DocumentTypeDefinition {
        name: "aspect",
        fields: ASPECT_FIELDS,
    },
    DocumentTypeDefinition {
        name: "character",
        fields: CHARACTER_FIELDS,
    },
    DocumentTypeDefinition {
        name: "deity",
        fields: DEITY_FIELDS,
    },
    DocumentTypeDefinition {
        name: "event",
        fields: EVENT_FIELDS,
    },
    DocumentTypeDefinition {
        name: "language",
        fields: LANGUAGE_FIELDS,
    },
    DocumentTypeDefinition {
        name: "location",
        fields: LOCATION_FIELDS,
    },
    DocumentTypeDefinition {
        name: "lore",
        fields: LORE_FIELDS,
    },
    DocumentTypeDefinition {
        name: "metagame",
        fields: METAGAME_FIELDS,
    },
    DocumentTypeDefinition {
        name: "monster",
        fields: MONSTER_FIELDS,
    },
    DocumentTypeDefinition {
        name: "object",
        fields: OBJECT_FIELDS,
    },
    DocumentTypeDefinition {
        name: "organisation",
        fields: ORGANISATION_FIELDS,
    },
    DocumentTypeDefinition {
        name: "race",
        fields: RACE_FIELDS,
    },
    DocumentTypeDefinition {
        name: "template",
        fields: TEMPLATE_FIELDS,
    },
];

pub const UNIVERSAL_FIELD_DEFINITIONS: &[FieldDefinition] = UNIVERSAL_FIELDS;

/// Overrides for fields whose shape differs by document type.
pub fn field_definition(note_type: &str, field_name: &str) -> Option<FieldDefinition> {
    if let Some(field) = UNIVERSAL_FIELDS
        .iter()
        .find(|field| field.name == field_name)
    {
        return Some(*field);
    }

    document_type_definition(note_type)?
        .fields
        .iter()
        .find(|field| field.name == field_name)
        .copied()
}

pub fn document_type_definition(note_type: &str) -> Option<&'static DocumentTypeDefinition> {
    DOCUMENT_TYPE_DEFINITIONS
        .iter()
        .find(|definition| definition.name == note_type)
}

pub fn field_is_declared_anywhere(field_name: &str) -> bool {
    UNIVERSAL_FIELDS
        .iter()
        .any(|field| field.name == field_name)
        || DOCUMENT_TYPE_DEFINITIONS
            .iter()
            .flat_map(|definition| definition.fields)
            .any(|field| field.name == field_name)
}

pub fn vocabulary_contains(vocabulary: &'static Vocabulary, value: &str) -> bool {
    vocabulary.values.contains(&value)
}

#[cfg(test)]
pub fn fixed_vocabulary(name: &str) -> Option<&'static Vocabulary> {
    match name {
        "document_types" => Some(&DOCUMENT_TYPES_VOCABULARY),
        "status" => Some(&STATUS),
        "visibility" => Some(&VISIBILITY),
        "adventure_status" => Some(&ADVENTURE_STATUS),
        "system" => Some(&SYSTEM),
        "character_role" => Some(&CHARACTER_ROLE),
        "life_status" => Some(&LIFE_STATUS),
        "deity_type" => Some(&DEITY_TYPE),
        "metagame_category" => Some(&METAGAME_CATEGORY),
        "historicity" => Some(&HISTORICITY),
        _ => None,
    }
}

/// The only currently declared cross-field constraint.
pub const EVENT_OCCURRENCE_CONFLICT: (&str, &[&str]) =
    ("occurred", &["occurred_start", "occurred_end"]);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defines_every_document_type_from_the_taxonomy() {
        assert_eq!(DOCUMENT_TYPE_DEFINITIONS.len(), DOCUMENT_TYPES.len());
        for document_type in DOCUMENT_TYPES {
            assert!(document_type_definition(document_type).is_some());
        }
    }

    #[test]
    fn applies_universal_defaults_and_requirements() {
        assert_eq!(
            field_definition("location", "aliases").map(|field| field.presence),
            Some(Presence::DefaultEmptyList)
        );
        assert_eq!(
            field_definition("location", "tags").map(|field| field.presence),
            Some(Presence::DefaultEmptyList)
        );
        assert_eq!(
            field_definition("location", "summary").map(|field| field.presence),
            Some(Presence::DefaultEmptyStringWithWarning)
        );
        assert_eq!(
            field_definition("location", "created").map(|field| field.presence),
            Some(Presence::Required)
        );
    }

    #[test]
    fn preserves_type_specific_shapes_and_overrides() {
        assert_eq!(
            field_definition("event", "occurred").map(|field| field.value_type),
            Some(ValueType::FantasyDate)
        );
        assert_eq!(
            field_definition("character", "patron_deities").map(|field| field.value_type),
            Some(ValueType::WikilinkList)
        );
        assert_eq!(
            field_definition("organisation", "patron_deities").map(|field| field.value_type),
            Some(ValueType::WikilinkList)
        );
    }

    #[test]
    fn distinguishes_fixed_and_extensible_vocabularies() -> anyhow::Result<()> {
        use anyhow::Context;

        let status = field_definition("location", "status")
            .context("status field must be defined for locations")?;
        assert_eq!(status.value_type, ValueType::FixedEnum(&STATUS));
        assert!(vocabulary_contains(&STATUS, "canon"));
        assert!(!vocabulary_contains(&STATUS, "future"));

        assert_eq!(
            field_definition("event", "event_type").map(|field| field.value_type),
            Some(ValueType::ExtensibleVocabulary)
        );
        Ok(())
    }

    #[test]
    fn exposes_the_event_occurrence_constraint() {
        assert_eq!(EVENT_OCCURRENCE_CONFLICT.0, "occurred");
        assert_eq!(
            EVENT_OCCURRENCE_CONFLICT.1,
            &["occurred_start", "occurred_end"]
        );
    }

    #[test]
    fn matches_every_runtime_field_and_vocabulary_in_the_migration_spec() -> anyhow::Result<()> {
        use anyhow::Context;

        let specification: toml::Value =
            toml::from_str(include_str!("../../../tmp/taxonomy.toml"))?;
        let universal_fields = specification
            .get("universal_fields")
            .and_then(toml::Value::as_table)
            .context("taxonomy universal_fields table")?;
        for field_name in universal_fields.keys() {
            assert!(
                UNIVERSAL_FIELDS
                    .iter()
                    .any(|field| field.name == field_name),
                "universal field `{field_name}` has no runtime definition"
            );
        }

        let type_fields = specification
            .get("type_fields")
            .and_then(toml::Value::as_table)
            .context("taxonomy type_fields table")?;
        for field_name in type_fields.keys() {
            assert!(
                field_is_declared_anywhere(field_name),
                "type field `{field_name}` has no runtime definition"
            );
        }

        let type_definitions = specification
            .get("types")
            .and_then(toml::Value::as_table)
            .context("taxonomy types table")?;
        for (type_name, definition) in type_definitions {
            let fields = definition
                .get("fields")
                .and_then(toml::Value::as_array)
                .context("taxonomy type fields list")?;
            let runtime = document_type_definition(type_name)
                .with_context(|| format!("runtime definition for `{type_name}`"))?;
            for field in fields {
                let field_name = field.as_str().context("taxonomy field name")?;
                assert!(
                    runtime.fields.iter().any(|field| field.name == field_name)
                        || field_definition(type_name, field_name).is_some(),
                    "field `{field_name}` is not defined for type `{type_name}`"
                );
            }
        }

        let enums = specification
            .get("enums")
            .and_then(toml::Value::as_table)
            .context("taxonomy enums table")?;
        for (name, values) in enums {
            let vocabulary =
                fixed_vocabulary(name).with_context(|| format!("runtime vocabulary `{name}`"))?;
            let expected = values
                .as_array()
                .context("taxonomy enum values")?
                .iter()
                .map(|value| value.as_str().context("taxonomy enum value"))
                .collect::<anyhow::Result<Vec<_>>>()?;
            assert_eq!(
                vocabulary.values,
                expected.as_slice(),
                "vocabulary `{name}`"
            );
        }
        Ok(())
    }
}
