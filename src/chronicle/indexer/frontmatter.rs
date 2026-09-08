use std::collections::{BTreeMap, HashSet};

use anyhow::{Context, Result, bail, ensure};
use chrono::NaiveDate;
use serde_yaml::{Mapping, Value};
use tracing::warn;

use super::schema::{self, FieldDefinition, Presence, ValueType};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetadataValue {
    String(String),
    StringList(Vec<String>),
    Boolean(bool),
    Date(String),
    FantasyDate(String),
    Wikilink(String),
    WikilinkList(Vec<String>),
    StringOrWikilink(String),
    Enum(String),
}

/// Parsed Chronicle frontmatter.
///
/// `fields` is the complete normalized representation of declared fields.
/// `unknown_fields` retains undeclared YAML values until the runtime schema is
/// extended. The `role` and `life_status` fields are retained for the current
/// structured-query implementation and mirror values in `fields`.
#[derive(Debug, Clone, Default)]
pub struct Metadata {
    pub id: String,
    pub note_type: String,
    pub aliases: Vec<String>,
    pub tags: Vec<String>,
    pub summary: String,
    pub status: String,
    pub visibility: String,
    pub created: String,
    pub updated: String,
    pub role: Option<crate::chronicle::query::plan::CharacterRole>,
    pub life_status: Option<crate::chronicle::query::plan::CharacterStatus>,
    pub fields: BTreeMap<String, MetadataValue>,
    #[allow(dead_code)]
    pub unknown_fields: BTreeMap<String, Value>,
}

/// Missing frontmatter is ineligible; malformed frontmatter is an ingestion
/// error. Unknown fields are preserved and warned about.
#[allow(clippy::too_many_lines)]
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

    let document: Value = serde_yaml::from_str(&yaml).context("Invalid Chronicle frontmatter")?;
    let mapping = document
        .as_mapping()
        .context("Chronicle frontmatter must be a YAML mapping")?;
    let note_type = required_string(mapping, "type")?;

    let mut fields = BTreeMap::new();
    let mut validation_errors = Vec::new();
    for field in schema::UNIVERSAL_FIELD_DEFINITIONS {
        if let Err(error) = parse_declared_field(mapping, field, &mut fields) {
            validation_errors.push(format!("{}: {error:#}", field.name));
        }
    }
    if let Some(type_definition) = schema::document_type_definition(&note_type) {
        for field in type_definition.fields {
            let field = schema::field_definition(&note_type, field.name)
                .with_context(|| format!("No schema definition for field `{}`", field.name))?;
            if let Err(error) = parse_declared_field(mapping, &field, &mut fields) {
                validation_errors.push(format!("{}: {error:#}", field.name));
            }
        }
    }

    let declared_names = mapping
        .keys()
        .filter_map(Value::as_str)
        .filter(|name| schema::field_definition(&note_type, name).is_some())
        .collect::<HashSet<_>>();
    let mut unknown_fields = BTreeMap::new();
    for (key, value) in mapping {
        let Some(name) = key.as_str() else {
            bail!("Chronicle frontmatter field names must be strings");
        };
        if declared_names.contains(name) {
            continue;
        }
        let applicability = if schema::field_is_declared_anywhere(name) {
            format!("Field `{name}` is not declared for document type `{note_type}`")
        } else {
            format!("Field `{name}` is not declared in the Chronicle schema")
        };
        warn!(
            field = name,
            value_kind = yaml_value_kind(value),
            "{applicability}; preserving value. Declare it in src/chronicle/indexer/schema.rs and recompile if it is intentional"
        );
        unknown_fields.insert(name.to_owned(), value.clone());
    }

    if let Err(error) = validate_event_occurrence_conflict(&fields, &note_type) {
        validation_errors.push(format!("event occurrence: {error:#}"));
    }
    if !validation_errors.is_empty() {
        bail!(
            "Chronicle frontmatter validation failed:\n{}",
            validation_errors
                .into_iter()
                .map(|error| format!("- {error}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    let id = required_non_empty_string(&fields, "id")?;
    let status = required_enum_string(&fields, "status")?;
    let visibility = required_enum_string(&fields, "visibility")?;
    let aliases = required_string_list(&fields, "aliases")?;
    let tags = required_string_list(&fields, "tags")?;
    let summary = required_string_value(&fields, "summary")?;
    let created = required_string_value(&fields, "created")?;
    let updated = required_string_value(&fields, "updated")?;

    Ok(Some((
        Metadata {
            id,
            note_type,
            aliases,
            tags,
            summary,
            status,
            visibility,
            created,
            updated,
            role: optional_character_role(&fields),
            life_status: optional_life_status(&fields),
            fields,
            unknown_fields,
        },
        lines.collect(),
    )))
}

fn parse_declared_field(
    mapping: &Mapping,
    field: &FieldDefinition,
    fields: &mut BTreeMap<String, MetadataValue>,
) -> Result<()> {
    let Some(raw) = mapping.get(Value::String(field.name.to_owned())) else {
        match field.presence {
            Presence::Required => bail!("Missing required frontmatter field `{}`", field.name),
            Presence::DefaultEmptyList => {
                fields.insert(field.name.to_owned(), MetadataValue::StringList(Vec::new()));
            }
            Presence::DefaultEmptyStringWithWarning => {
                warn!(
                    field = field.name,
                    "Missing Chronicle frontmatter summary; defaulting to an empty string"
                );
                fields.insert(field.name.to_owned(), MetadataValue::String(String::new()));
            }
            Presence::Optional => {}
        }
        return Ok(());
    };

    if raw.is_null() || raw.as_str().is_some_and(|value| value.trim().is_empty()) {
        match field.presence {
            Presence::Required => {
                bail!("Required frontmatter field `{}` cannot be null", field.name)
            }
            Presence::DefaultEmptyList => {
                fields.insert(field.name.to_owned(), MetadataValue::StringList(Vec::new()));
            }
            Presence::DefaultEmptyStringWithWarning => {
                warn!(
                    field = field.name,
                    "Missing Chronicle frontmatter summary; defaulting to an empty string"
                );
                fields.insert(field.name.to_owned(), MetadataValue::String(String::new()));
            }
            Presence::Optional => {}
        }
        return Ok(());
    }

    let value = parse_value(field, raw).with_context(|| {
        format!(
            "Field `{}` has invalid value (expected {})",
            field.name,
            expected_value_type(field.value_type)
        )
    })?;
    fields.insert(field.name.to_owned(), value);
    Ok(())
}

fn parse_value(field: &FieldDefinition, raw: &Value) -> Result<MetadataValue> {
    match field.value_type {
        ValueType::String | ValueType::ExtensibleVocabulary => {
            Ok(MetadataValue::String(required_yaml_string(raw)?))
        }
        ValueType::StringList => Ok(MetadataValue::StringList(
            required_yaml_string_list(raw)?
                .into_iter()
                .filter(|value| !value.trim().is_empty())
                .collect(),
        )),
        ValueType::Boolean => raw
            .as_bool()
            .map(MetadataValue::Boolean)
            .context("expected a boolean"),
        ValueType::Date => {
            let value = required_yaml_string(raw)?;
            NaiveDate::parse_from_str(&value, "%Y-%m-%d")
                .with_context(|| format!("expected ISO date YYYY-MM-DD, got `{value}`"))?;
            Ok(MetadataValue::Date(value))
        }
        ValueType::FantasyDate => Ok(MetadataValue::FantasyDate(required_yaml_string(raw)?)),
        ValueType::Wikilink => {
            let value = required_yaml_string(raw)?;
            validate_wikilink(&value)?;
            Ok(MetadataValue::Wikilink(value))
        }
        ValueType::WikilinkList => {
            let values = required_yaml_string_list(raw)?
                .into_iter()
                .filter(|value| !value.trim().is_empty())
                .collect::<Vec<_>>();
            for value in &values {
                validate_wikilink(value)?;
            }
            Ok(MetadataValue::WikilinkList(values))
        }
        ValueType::StringOrWikilink => {
            let value = required_yaml_string(raw)?;
            if value.trim_start().starts_with("[[") {
                validate_wikilink(&value)?;
            }
            Ok(MetadataValue::StringOrWikilink(value))
        }
        ValueType::FixedEnum(vocabulary) => {
            let value = required_yaml_string(raw)?;
            ensure!(
                schema::vocabulary_contains(vocabulary, &value),
                "Invalid fixed-enum value `{value}` for `{}`; allowed values are [{}]. Add the value to the vocabulary in src/chronicle/indexer/schema.rs and recompile",
                vocabulary.name,
                vocabulary.values.join(", ")
            );
            Ok(MetadataValue::Enum(value))
        }
    }
}

fn required_string(mapping: &Mapping, field: &str) -> Result<String> {
    let value = mapping
        .get(Value::String(field.to_owned()))
        .with_context(|| format!("Missing required frontmatter field `{field}`"))?;
    required_yaml_string(value)
        .with_context(|| format!("Frontmatter field `{field}` must be a string"))
}

fn required_yaml_string(value: &Value) -> Result<String> {
    value
        .as_str()
        .map(ToOwned::to_owned)
        .context("expected a string")
}

fn expected_value_type(value_type: ValueType) -> &'static str {
    match value_type {
        ValueType::String => "a string",
        ValueType::StringList => "a list of strings",
        ValueType::Boolean => "a boolean",
        ValueType::Date => "an ISO date (YYYY-MM-DD)",
        ValueType::FantasyDate => "a fantasy-date string",
        ValueType::Wikilink => "a wikilink such as [[Target]]",
        ValueType::WikilinkList => "a list of wikilinks such as [[Target]]",
        ValueType::StringOrWikilink => "a string or wikilink such as [[Target]]",
        ValueType::FixedEnum(vocabulary) => vocabulary.name,
        ValueType::ExtensibleVocabulary => "a string vocabulary value",
    }
}

fn yaml_value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Sequence(_) => "list",
        Value::Mapping(_) => "mapping",
        Value::Tagged(_) => "tagged value",
    }
}

fn required_yaml_string_list(value: &Value) -> Result<Vec<String>> {
    value
        .as_sequence()
        .context("expected a list")?
        .iter()
        .map(|item| required_yaml_string(item).context("list items must be strings"))
        .collect()
}

fn required_non_empty_string(
    fields: &BTreeMap<String, MetadataValue>,
    field: &str,
) -> Result<String> {
    let value = required_string_value(fields, field)?;
    ensure!(
        !value.trim().is_empty(),
        "Frontmatter `{field}` must be non-empty"
    );
    Ok(value)
}

fn required_string_value(fields: &BTreeMap<String, MetadataValue>, field: &str) -> Result<String> {
    match fields.get(field) {
        Some(
            MetadataValue::String(value)
            | MetadataValue::Date(value)
            | MetadataValue::Enum(value)
            | MetadataValue::StringOrWikilink(value),
        ) => Ok(value.clone()),
        _ => bail!("Frontmatter field `{field}` must be a scalar string"),
    }
}

fn required_enum_string(fields: &BTreeMap<String, MetadataValue>, field: &str) -> Result<String> {
    match fields.get(field) {
        Some(MetadataValue::Enum(value)) => Ok(value.clone()),
        _ => bail!("Frontmatter field `{field}` must be a fixed enum"),
    }
}

fn required_string_list(
    fields: &BTreeMap<String, MetadataValue>,
    field: &str,
) -> Result<Vec<String>> {
    match fields.get(field) {
        Some(MetadataValue::StringList(values)) => Ok(values.clone()),
        _ => bail!("Frontmatter field `{field}` must be a string list"),
    }
}

fn optional_character_role(
    fields: &BTreeMap<String, MetadataValue>,
) -> Option<crate::chronicle::query::plan::CharacterRole> {
    match fields.get("role") {
        Some(MetadataValue::Enum(value)) => match value.as_str() {
            "pc" => Some(crate::chronicle::query::plan::CharacterRole::Pc),
            "npc" => Some(crate::chronicle::query::plan::CharacterRole::Npc),
            "ex-pc" => Some(crate::chronicle::query::plan::CharacterRole::ExPc),
            _ => None,
        },
        _ => None,
    }
}

fn optional_life_status(
    fields: &BTreeMap<String, MetadataValue>,
) -> Option<crate::chronicle::query::plan::CharacterStatus> {
    match fields.get("life_status") {
        Some(MetadataValue::Enum(value)) => match value.as_str() {
            "alive" => Some(crate::chronicle::query::plan::CharacterStatus::Alive),
            "dead" => Some(crate::chronicle::query::plan::CharacterStatus::Dead),
            "missing" => Some(crate::chronicle::query::plan::CharacterStatus::Missing),
            "unknown" => Some(crate::chronicle::query::plan::CharacterStatus::Unknown),
            _ => None,
        },
        _ => None,
    }
}

fn validate_wikilink(value: &str) -> Result<()> {
    let trimmed = value.trim();
    ensure!(
        trimmed.starts_with("[[") && trimmed.ends_with("]]"),
        "expected an Obsidian wikilink such as `[[Target]]`"
    );
    ensure!(
        trimmed.len() > 4 && !trimmed[2..trimmed.len() - 2].trim().is_empty(),
        "wikilink target cannot be empty"
    );
    Ok(())
}

/// An optional field is omitted from `fields` when its YAML value is null or a
/// blank string. Validate against that normalized representation so note
/// templates may include all three occurrence keys without creating a false
/// conflict.
fn validate_event_occurrence_conflict(
    fields: &BTreeMap<String, MetadataValue>,
    note_type: &str,
) -> Result<()> {
    if note_type != "event" {
        return Ok(());
    }
    let (field, conflicts) = schema::EVENT_OCCURRENCE_CONFLICT;
    if fields.contains_key(field) {
        for conflict in conflicts {
            ensure!(
                !fields.contains_key(*conflict),
                "Frontmatter fields `{field}` and `{conflict}` cannot be used together"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(extra: &str) -> String {
        format!(
            "---\nid: test\ntype: character\nstatus: canon\nvisibility: player\ncreated: 2026-09-07\nupdated: 2026-09-07\n{extra}---\n# Story\nHello"
        )
    }

    #[test]
    fn parses_full_values_and_preserves_unknown_fields() -> Result<()> {
        let (metadata, body) = parse(&note(
            "aliases: [Someone]\ntags: [npc, garden]\nsummary: A gardener\nrace: '[[Human]]'\nrole: npc\nlife_status: alive\nallies: ['[[Ember Guild]]']\nplayed_by: Ada\ncustom: [one, two]\n",
        ))?
        .context("Expected parsed note")?;
        assert_eq!(metadata.id, "test");
        assert_eq!(metadata.aliases, ["Someone"]);
        assert_eq!(metadata.tags, ["npc", "garden"]);
        assert_eq!(metadata.created, "2026-09-07");
        assert_eq!(metadata.updated, "2026-09-07");
        assert_eq!(
            metadata.fields.get("race"),
            Some(&MetadataValue::Wikilink("[[Human]]".into()))
        );
        assert!(metadata.unknown_fields.contains_key("custom"));
        assert_eq!(body, "# Story\nHello");
        Ok(())
    }

    #[test]
    fn defaults_aliases_and_tags_and_warns_for_missing_summary() -> Result<()> {
        let (metadata, _) = parse(&note(""))?.context("Expected parsed note")?;
        assert!(metadata.aliases.is_empty());
        assert!(metadata.tags.is_empty());
        assert!(metadata.summary.is_empty());
        Ok(())
    }

    #[test]
    fn requires_non_default_universal_fields() {
        for field in ["id", "type", "status", "visibility", "created", "updated"] {
            assert!(
                parse(&note(&format!("{field}:\n")).replace("id: test", "id: ")).is_err(),
                "{field}"
            );
        }
    }

    #[test]
    fn validates_fixed_enums_shapes_dates_and_wikilinks() {
        assert!(parse(&note("role: villain\n")).is_err());
        assert!(parse(&note("created: 2026-99-99\n")).is_err());
        assert!(parse(&note("race: Human\n")).is_err());
    }

    #[test]
    fn parses_all_declared_value_shapes_for_each_document_type() -> Result<()> {
        let cases = [
            (
                "adventure",
                "adventure_status: completed\nstart_date: 2026-01-01\nend_date: 2026-01-02\nparty: ['[[Ada]]']\nregions: ['[[Northmere]]']\nrelated_events: ['[[Treaty]]']\nsystem: 5e\npart_of_adventure: '[[Campaign]]'\nlevel_range: 1-5\nantagonists: ['[[Dragon]]']\n",
            ),
            (
                "aspect",
                "ruling_deities: ['[[Aurelia]]']\nnative_races: ['[[Human]]']\n",
            ),
            (
                "character",
                "race: '[[Human]]'\nrole: npc\nlife_status: alive\nlife_status_cause: old_age\nlife_status_since: \"1608\"\nappearances: ['[[The Long Road]]']\naffiliations: ['[[Guild]]']\nallies: ['[[Ada]]']\nenemies: ['[[Orc]]']\nparents: ['[[Parent]]']\nsiblings: ['[[Sibling]]']\nchildren: ['[[Child]]']\npartners: ['[[Partner]]']\nother_family: ['[[Family]]']\nlocation: '[[Northmere]]'\npatron_deities: ['[[Aurelia]]']\nbirthplace: '[[Northmere]]'\nbirth_year: \"1560\"\nnationality: Northmerian\nplayed_by: Player\npronouns: they/them\n",
            ),
            (
                "deity",
                "deity_type: Major\ndomain: life\nantidomain: death\nalignment: good\nform: humanoid\ncrystal: blue\nrival_deities: ['[[Veyra]]']\nworshippers: ['[[Ember Guild]]']\nholy_sites: ['[[Moonspire]]']\nassociated_aspects: ['[[Harvest]]']\n",
            ),
            (
                "event",
                "event_type: treaty\noccurred: 418 NY\nlocations: ['[[Alderwatch]]']\nparticipants: ['[[Tovan]]']\ncauses: ['[[Dispute]]']\nconsequences: ['[[Peace]]']\nhistoricity: historical\naffected_regions: ['[[Northmere]]']\nresult: settled\n",
            ),
            (
                "language",
                "speakers: ['[[Humans]]']\nscripts: [Common, Runes]\n",
            ),
            (
                "location",
                "location_type: city\ncontained_in: '[[Northmere]]'\npolitical_affiliations: ['[[Kingdom]]']\npopulation: many\ndemonym: Northmerian\n",
            ),
            ("lore", "lore_type: tradition\ncommon_knowledge: true\n"),
            (
                "metagame",
                "category: mechanic\nsystem: draw-steel\nsession_date: 2026-01-03\n",
            ),
            (
                "monster",
                "creature_type: dragon\nhabitat: ['[[Mountain]]']\nthreat_level: high\nalignment: evil\nfactions: ['[[Horde]]']\nweaknesses: [cold, silence]\nsizes: [huge]\nsource_inspiration: folklore\nnotable_examples: ['[[Smaug]]']\n",
            ),
            (
                "object",
                "object_type: weapon\nrarity: rare\nowner: '[[Ada]]'\nlocation: '[[Vault]]'\ncreator: '[[Smith]]'\nattunement: wizard\n",
            ),
            (
                "organisation",
                "organisation_type: guild\nleader: '[[Tovan]]'\nfounder: '[[Ada]]'\nmembers: ['[[Ada]]']\nallies: ['[[Kingdom]]']\nenemies: ['[[Horde]]']\nheadquarters: '[[Alderwatch]]'\nfounded: 400 NY\npatron_deities: ['[[Aurelia]]', '[[Veyra]]']\ndissolved: 500 NY\njurisdiction: ['[[Northmere]]']\nideology: [craft, trade]\nmotto: Light for all\n",
            ),
            (
                "race",
                "homeland: ['[[Northmere]]']\nlifespan: 80 years\nplayable: true\nrelated_organisations: ['[[Guild]]']\nlanguages: ['[[Common]]']\nsubraces: ['[[Highland]]']\nsizes: [medium]\n",
            ),
            ("template", ""),
        ];
        for (note_type, fields) in cases {
            let source = format!(
                "---\nid: {note_type}\ntype: {note_type}\nstatus: canon\nvisibility: player\ncreated: 2026-09-07\nupdated: 2026-09-07\n{fields}---\n"
            );
            parse(&source)?.context(note_type)?;
        }
        Ok(())
    }

    #[test]
    fn accepts_every_value_in_each_fixed_vocabulary() -> Result<()> {
        let cases = [
            ("type", "location", "document_types"),
            ("status", "location", "status"),
            ("visibility", "location", "visibility"),
            ("adventure_status", "adventure", "adventure_status"),
            ("system", "adventure", "system"),
            ("role", "character", "character_role"),
            ("life_status", "character", "life_status"),
            ("deity_type", "deity", "deity_type"),
            ("category", "metagame", "metagame_category"),
            ("historicity", "event", "historicity"),
        ];
        for (field, note_type, vocabulary_name) in cases {
            let vocabulary = schema::fixed_vocabulary(vocabulary_name)
                .with_context(|| format!("vocabulary `{vocabulary_name}`"))?;
            for value in vocabulary.values {
                let actual_type = if field == "type" { value } else { note_type };
                let extra = if matches!(field, "type" | "status" | "visibility") {
                    String::new()
                } else {
                    format!("{field}: {value}\n")
                };
                let mut source = format!(
                    "---\nid: enum-test\ntype: {actual_type}\nstatus: canon\nvisibility: player\ncreated: 2026-09-07\nupdated: 2026-09-07\n{extra}---\n"
                );
                if field == "status" {
                    source = source.replace("status: canon", &format!("status: {value}"));
                } else if field == "visibility" {
                    source = source.replace("visibility: player", &format!("visibility: {value}"));
                }
                parse(&source)?.with_context(|| format!("{field}={value}"))?;
            }
        }
        Ok(())
    }

    #[test]
    fn reports_all_invalid_fixed_enum_values_in_one_note() -> Result<()> {
        let source = "---\nid: deity\ntype: deity\nstatus: imaginary\nvisibility: everyone\ncreated: 2026-09-07\nupdated: 2026-09-07\ndeity_type: Demi-god\n---\n";
        let Err(error) = parse(source) else {
            bail!("invalid fixed enums must fail ingestion");
        };
        let rendered = format!("{error:#}");
        assert!(rendered.contains("imaginary"));
        assert!(rendered.contains("everyone"));
        assert!(rendered.contains("Demi-god"));
        assert!(rendered.contains("DEITY_TYPE") || rendered.contains("deity_type"));
        Ok(())
    }

    #[test]
    fn accepts_extensible_vocabularies() -> Result<()> {
        let source = "---\nid: event\ntype: event\nstatus: canon\nvisibility: player\ncreated: 2026-09-07\nupdated: 2026-09-07\nevent_type: eclipse\n---\n";
        let (metadata, _) = parse(source)?.context("Expected parsed note")?;
        assert_eq!(
            metadata.fields.get("event_type"),
            Some(&MetadataValue::String("eclipse".into()))
        );
        Ok(())
    }

    #[test]
    fn rejects_conflicting_event_dates() {
        let source = "---\nid: event\ntype: event\nstatus: canon\nvisibility: player\ncreated: 2026-09-07\nupdated: 2026-09-07\noccurred: 418 NY\noccurred_start: 418 NY\n---\n";
        assert!(parse(source).is_err());
    }

    #[test]
    fn accepts_empty_event_occurrence_template_fields() -> Result<()> {
        let source = "---\nid: event\ntype: event\nstatus: canon\nvisibility: player\ncreated: 2026-09-07\nupdated: 2026-09-07\noccurred: \"\"\noccurred_start:\noccurred_end: \"  \"\n---\n";

        let (metadata, _) = parse(source)?.context("Expected parsed note")?;
        assert!(!metadata.fields.contains_key("occurred"));
        assert!(!metadata.fields.contains_key("occurred_start"));
        assert!(!metadata.fields.contains_key("occurred_end"));
        Ok(())
    }

    #[test]
    fn preserves_type_specific_fields_on_other_types_as_unknown() -> Result<()> {
        let (metadata, _) = parse(&note("pantheon: major\n"))?.context("Expected parsed note")?;
        assert!(metadata.unknown_fields.contains_key("pantheon"));
        Ok(())
    }

    #[test]
    fn separates_metadata_and_body() -> Result<()> {
        let source = "---\r\nid: person\r\ntype: location\r\nstatus: canon\r\nvisibility: secret\r\ncreated: 2026-09-07\r\nupdated: 2026-09-07\r\naliases: [Someone]\r\nupdated_by_plugin: 2026-09-07\r\n---\r\n# Story\r\nHello";
        let (meta, body) = parse(source)?.context("Expected parsed note")?;
        assert_eq!(meta.id, "person");
        assert_eq!(body, "# Story\r\nHello");
        assert!(meta.unknown_fields.contains_key("updated_by_plugin"));
        assert!(parse("no frontmatter")?.is_none());
        assert!(parse("---\nid: broken").is_err());
        assert!(parse("---\nid: 123\n---").is_err());
        Ok(())
    }
}
