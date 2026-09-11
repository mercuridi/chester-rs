use anyhow::{Context, Result};
use sqlx::Row;
use std::{collections::HashSet, fs, path::Path};

use super::facade::{versioned_database_url, *};
use crate::chronicle::indexer::link_resolver::{
    self, LinkOrigin, LinkResolution, LinkVisibility, ResolvedLink,
};
use crate::chronicle::indexer::scanner::{self, DocumentCandidate};
use tempfile::tempdir;

fn embedding(value: f32) -> Vec<f32> {
    vec![value; crate::chronicle::indexer::embedder::EMBEDDING_DIMENSIONS]
}

fn resolve_candidates(root: &Path, candidates: &[DocumentCandidate]) -> Result<LinkResolution> {
    let catalogue = link_resolver::catalogue_from_candidates(root, candidates)?;
    let mut outcome = LinkResolution::default();
    for candidate in candidates {
        let document = scanner::load_document(candidate)?;
        let resolved = link_resolver::resolve_document(&catalogue, &document)?;
        outcome.resolved.extend(resolved.resolved);
        outcome.dangling.extend(resolved.dangling);
        outcome.ambiguous.extend(resolved.ambiguous);
    }
    Ok(outcome)
}

fn chunks() -> Vec<IndexedChunk> {
    vec![
        IndexedChunk {
            chunk_index: 0,
            heading: Some("Introduction".into()),
            text: "First chunk".into(),
            visibility: crate::chronicle::indexer::document::ChunkVisibility::Player,
            overlaps_previous: false,
        },
        IndexedChunk {
            chunk_index: 1,
            heading: Some("Introduction".into()),
            text: "Second chunk".into(),
            visibility: crate::chronicle::indexer::document::ChunkVisibility::Player,
            overlaps_previous: true,
        },
    ]
}

async fn test_database() -> anyhow::Result<(tempfile::TempDir, IndexerDb)> {
    let directory = tempdir()?;
    let url = format!(
        "sqlite://{}",
        directory.path().join("chronicle.db").display()
    );
    Ok((directory, IndexerDb::open(&url).await?))
}

#[test]
fn versioned_database_filename_selects_the_current_index_format() {
    assert_eq!(
        versioned_database_url("sqlite:///data/chronicle.sqlite3?mode=rwc"),
        format!(
            "sqlite:///data/chronicle.index-v{}.sqlite3?mode=rwc",
            super::super::schema::INDEX_FORMAT_VERSION
        )
    );
    assert_eq!(
        versioned_database_url("sqlite://:memory:"),
        "sqlite://:memory:"
    );
}

#[tokio::test]
async fn structured_lists_are_capped_but_counts_are_distinct_and_complete() -> Result<()> {
    let (_directory, db) = test_database().await?;
    let (mut metadata, _) = crate::chronicle::indexer::frontmatter::parse("---\nid: initial\ntype: character\nstatus: canon\nvisibility: player\ncreated: 2026-09-07\nupdated: 2026-09-07\nrole: npc\n---\n")?.context("note")?;
    for i in 0..25 {
        metadata.id = format!("id-{i:02}");
        db.replace_note(&format!("Note {i}.md"), "hash", &[], &[], &metadata)
            .await?;
    }
    db.replace_note("Duplicate.md", "hash", &[], &[], &metadata)
        .await?;
    let plan = crate::chronicle::query::plan::StructuredPlan::try_from(
        crate::chronicle::query::planner::parse(
            r#"{"operation":"list","note_type":"character","filters":{"conditions":[{"field":"role","operator":"equals","value":"npc"}]}}"#,
        )?,
    )?;
    let result = db.execute_plan_for(&plan, AccessScope::Gm).await?;
    assert_eq!(result.total, 25);
    assert_eq!(result.notes.len(), 20);
    assert_eq!(result.notes[0].id, "id-00");
    assert_eq!(result.notes[19].id, "id-19");
    Ok(())
}

#[tokio::test]
async fn type_specific_metadata_round_trips_and_replaces_lists() -> Result<()> {
    let (_directory, db) = test_database().await?;
    let source = "---\nid: ember-guild\ntype: organisation\nstatus: canon\nvisibility: player\ncreated: 2026-09-07\nupdated: 2026-09-07\norganisation_type: guild\nleader: '[[Tovan]]'\npatron_deities: ['[[Aurelia]]', '[[Veyra]]']\nideology: [craft, mutual-aid]\n---\n";
    let (metadata, _) = crate::chronicle::indexer::frontmatter::parse(source)?.context("note")?;
    let document_id = db
        .replace_note("Ember Guild.md", "hash", &[], &[], &metadata)
        .await?;

    let row = sqlx::query(
        "SELECT organisation_type, leader, motto FROM organisation_metadata WHERE document_id = ?",
    )
    .bind(document_id)
    .fetch_one(&db.pool)
    .await?;
    assert_eq!(row.get::<String, _>("organisation_type"), "guild");
    assert_eq!(row.get::<String, _>("leader"), "[[Tovan]]");
    assert!(row.get::<Option<String>, _>("motto").is_none());

    let links = sqlx::query("SELECT field_name, position, value FROM note_wikilinks WHERE document_id = ? ORDER BY field_name, position")
            .bind(document_id).fetch_all(&db.pool).await?;
    assert_eq!(links.len(), 2);
    assert_eq!(links[0].get::<String, _>("field_name"), "patron_deities");
    assert_eq!(links[0].get::<i64, _>("position"), 0);
    assert_eq!(links[0].get::<String, _>("value"), "[[Aurelia]]");
    assert_eq!(links[1].get::<String, _>("value"), "[[Veyra]]");

    let strings = sqlx::query("SELECT field_name, position, value FROM note_string_lists WHERE document_id = ? ORDER BY position")
            .bind(document_id).fetch_all(&db.pool).await?;
    assert_eq!(strings.len(), 2);
    assert_eq!(strings[0].get::<String, _>("value"), "craft");
    assert_eq!(strings[1].get::<String, _>("value"), "mutual-aid");

    let replacement = source.replace("'[[Aurelia]]', '[[Veyra]]'", "'[[Veyra]]'");
    let (metadata, _) =
        crate::chronicle::indexer::frontmatter::parse(&replacement)?.context("note")?;
    db.replace_note("Ember Guild.md", "hash-2", &[], &[], &metadata)
        .await?;
    let links =
        sqlx::query("SELECT value FROM note_wikilinks WHERE document_id = ? ORDER BY position")
            .bind(document_id)
            .fetch_all(&db.pool)
            .await?;
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].get::<String, _>("value"), "[[Veyra]]");
    Ok(())
}

#[tokio::test]
async fn rebuild_document_graph_persists_resolved_provenance_and_replaces_stale_edges() -> Result<()>
{
    let (_directory, db) = test_database().await?;
    for id in ["source", "target"] {
        let source = format!(
            "---\nid: {id}\ntype: character\nstatus: canon\nvisibility: player\ncreated: 2026-09-07\nupdated: 2026-09-07\n---\n"
        );
        let (metadata, _) =
            crate::chronicle::indexer::frontmatter::parse(&source)?.context("note")?;
        db.replace_note(&format!("{id}.md"), id, &[], &[], &metadata)
            .await?;
    }
    let resolution = LinkResolution {
        resolved: vec![
            ResolvedLink {
                source_note_id: "source".into(),
                target_note_id: "target".into(),
                origin: LinkOrigin::Frontmatter {
                    field_name: "affiliations".into(),
                },
                visibility: LinkVisibility::Player,
                raw: "[[target]]".into(),
                fragment: None,
            },
            ResolvedLink {
                source_note_id: "source".into(),
                target_note_id: "target".into(),
                origin: LinkOrigin::Frontmatter {
                    field_name: "affiliations".into(),
                },
                visibility: LinkVisibility::Player,
                raw: "[[target|the target]]".into(),
                fragment: None,
            },
            ResolvedLink {
                source_note_id: "source".into(),
                target_note_id: "target".into(),
                origin: LinkOrigin::Body,
                visibility: LinkVisibility::Secret,
                raw: "[[target#Hidden]]".into(),
                fragment: Some("Hidden".into()),
            },
        ],
        ..LinkResolution::default()
    };
    assert_eq!(db.rebuild_document_graph(&resolution).await?.edge_count, 2);
    let rows = sqlx::query("SELECT origin, field_name, visibility FROM document_graph_edges ORDER BY origin, field_name, visibility")
            .fetch_all(&db.pool).await?;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get::<String, _>("origin"), "body");
    assert_eq!(rows[0].get::<String, _>("field_name"), "");
    assert_eq!(rows[0].get::<String, _>("visibility"), "secret");
    assert_eq!(rows[1].get::<String, _>("origin"), "frontmatter");
    assert_eq!(rows[1].get::<String, _>("field_name"), "affiliations");
    assert_eq!(rows[1].get::<String, _>("visibility"), "player");

    assert_eq!(
        db.rebuild_document_graph(&LinkResolution::default())
            .await?
            .edge_count,
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM document_graph_edges")
            .fetch_one(&db.pool)
            .await?,
        0
    );
    Ok(())
}

#[tokio::test]
async fn resolver_output_persists_body_and_frontmatter_provenance_with_visibility() -> Result<()> {
    let (directory, db) = test_database().await?;
    let corpus = directory.path().join("corpus");
    fs::create_dir(&corpus)?;
    fs::write(
        corpus.join("Target.md"),
        "---\nid: target\ntype: character\nstatus: canon\nvisibility: player\ncreated: 2026-09-07\nupdated: 2026-09-07\n---\n",
    )?;
    fs::write(
        corpus.join("Source.md"),
        "---\nid: source\ntype: character\nstatus: canon\nvisibility: mixed\ncreated: 2026-09-07\nupdated: 2026-09-07\nlocation: '[[Target]]'\n---\nPublic [[Target]].\n\n> [!secret] Private\n> Secret [[Target#Hidden]].\n",
    )?;

    let (candidates, _) =
        scanner::discover_directory_with_stats_excluding(&corpus, &HashSet::new())?;
    let resolution = resolve_candidates(&corpus, &candidates)?;
    assert_eq!(resolution.resolved.len(), 3);
    for candidate in &candidates {
        let document = scanner::load_document(candidate)?;
        db.replace_note(
            &document.path.to_string_lossy(),
            &document.content_hash,
            &[],
            &[],
            &document.metadata,
        )
        .await?;
    }

    assert_eq!(db.rebuild_document_graph(&resolution).await?.edge_count, 3);
    let rows = sqlx::query(
            "SELECT origin, field_name, visibility FROM document_graph_edges ORDER BY origin, field_name, visibility",
        )
        .fetch_all(&db.pool)
        .await?;
    let actual = rows
        .iter()
        .map(|row| {
            (
                row.get::<String, _>("origin"),
                row.get::<String, _>("field_name"),
                row.get::<String, _>("visibility"),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        vec![
            ("body".into(), String::new(), "player".into()),
            ("body".into(), String::new(), "secret".into()),
            ("frontmatter".into(), "location".into(), "player".into()),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn excluded_dangling_and_ambiguous_links_persist_no_graph_edge() -> Result<()> {
    let (directory, db) = test_database().await?;
    let corpus = directory.path().join("corpus");
    fs::create_dir(&corpus)?;
    for (name, source) in [
        (
            "Source.md",
            "---\nid: source\ntype: character\nstatus: canon\nvisibility: player\ncreated: 2026-09-07\nupdated: 2026-09-07\n---\n[[Missing]] [[Shared]] [[excluded]]\n",
        ),
        (
            "Alias A.md",
            "---\nid: alias-a\ntype: character\nstatus: canon\nvisibility: player\ncreated: 2026-09-07\nupdated: 2026-09-07\naliases: [Shared]\n---\n",
        ),
        (
            "Alias B.md",
            "---\nid: alias-b\ntype: character\nstatus: canon\nvisibility: player\ncreated: 2026-09-07\nupdated: 2026-09-07\naliases: [Shared]\n---\n",
        ),
        (
            "Excluded.md",
            "---\nid: excluded\ntype: character\nstatus: canon\nvisibility: player\ncreated: 2026-09-07\nupdated: 2026-09-07\n---\n",
        ),
    ] {
        fs::write(corpus.join(name), source)?;
    }

    let excluded = HashSet::from(["excluded".to_owned()]);
    let (candidates, _) = scanner::discover_directory_with_stats_excluding(&corpus, &excluded)?;
    let resolution = resolve_candidates(&corpus, &candidates)?;
    assert!(resolution.resolved.is_empty());
    assert_eq!(resolution.dangling.len(), 2);
    assert_eq!(resolution.ambiguous.len(), 1);
    for candidate in &candidates {
        let document = scanner::load_document(candidate)?;
        db.replace_note(
            &document.path.to_string_lossy(),
            &document.content_hash,
            &[],
            &[],
            &document.metadata,
        )
        .await?;
    }

    assert_eq!(db.rebuild_document_graph(&resolution).await?.edge_count, 0);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM document_graph_edges")
            .fetch_one(&db.pool)
            .await?,
        0
    );
    Ok(())
}

#[tokio::test]
async fn rebuild_document_pagerank_separates_player_and_gm_graphs() -> Result<()> {
    let (_directory, db) = test_database().await?;
    for (id, visibility) in [
        ("player-source", "player"),
        ("target", "player"),
        ("secret-source", "secret"),
    ] {
        let source = format!(
            "---\nid: {id}\ntype: character\nstatus: canon\nvisibility: {visibility}\ncreated: 2026-09-07\nupdated: 2026-09-07\n---\n"
        );
        let (metadata, _) =
            crate::chronicle::indexer::frontmatter::parse(&source)?.context("note")?;
        db.replace_note(&format!("{id}.md"), id, &[], &[], &metadata)
            .await?;
    }
    db.rebuild_document_graph(&LinkResolution {
        resolved: vec![
            ResolvedLink {
                source_note_id: "player-source".into(),
                target_note_id: "target".into(),
                origin: LinkOrigin::Body,
                visibility: LinkVisibility::Player,
                raw: "[[target]]".into(),
                fragment: None,
            },
            ResolvedLink {
                source_note_id: "secret-source".into(),
                target_note_id: "target".into(),
                origin: LinkOrigin::Body,
                visibility: LinkVisibility::Secret,
                raw: "[[target]]".into(),
                fragment: None,
            },
        ],
        ..LinkResolution::default()
    })
    .await?;
    let stats = db.rebuild_document_pagerank().await?;
    assert_eq!(stats.document_count, 3);
    assert!(stats.player_iterations > 0);
    assert!(stats.gm_iterations > 0);

    let rows = sqlx::query(
        "SELECT m.note_id, p.player_score, p.player_rank, p.gm_score, p.gm_rank
             FROM document_pagerank p JOIN note_metadata m ON m.document_id = p.document_id
             ORDER BY m.note_id",
    )
    .fetch_all(&db.pool)
    .await?;
    let score = |id: &str, column: &str| -> Result<f64> {
        rows.iter()
            .find(|row| row.get::<String, _>("note_id") == id)
            .map(|row| row.get(column))
            .context("score row exists")
    };
    let rank = |id: &str, column: &str| -> Result<i64> {
        rows.iter()
            .find(|row| row.get::<String, _>("note_id") == id)
            .map(|row| row.get(column))
            .context("rank row exists")
    };
    assert!(score("secret-source", "player_score")?.abs() < f64::EPSILON);
    assert_eq!(rank("secret-source", "player_rank")?, 0);
    assert_eq!(rank("target", "player_rank")?, 1);
    assert_eq!(rank("target", "gm_rank")?, 1);
    assert!(score("target", "player_score")? > score("player-source", "player_score")?);
    assert!(score("target", "gm_score")? > score("secret-source", "gm_score")?);
    Ok(())
}

#[tokio::test]
async fn structured_conditions_query_scalar_and_wikilink_list_metadata() -> Result<()> {
    let (_directory, db) = test_database().await?;
    let source = "---\nid: vex\ntype: character\nstatus: canon\nvisibility: player\ncreated: 2026-09-07\nupdated: 2026-09-07\nlife_status: dead\nlife_status_cause: '[[Great Dungeon Fight]]'\nappearances: ['[[Blueskies]]']\n---\n";
    let (metadata, _) = crate::chronicle::indexer::frontmatter::parse(source)?.context("note")?;
    db.replace_note("Vex.md", "hash", &[], &[], &metadata)
        .await?;

    let plan = crate::chronicle::query::plan::StructuredPlan::try_from(
        crate::chronicle::query::planner::parse(
            r#"{"operation":"list","note_type":"character","filters":{"conditions":[{"field":"life_status_cause","operator":"equals","value":"[[Great Dungeon Fight]]"},{"field":"appearances","operator":"contains","value":"[[Blueskies]]"}]}}"#,
        )?,
    )?;
    let result = db.execute_plan_for(&plan, AccessScope::Gm).await?;
    assert_eq!(result.total, 1);
    assert_eq!(result.notes[0].id, "vex");

    let mut plain_target_plan = crate::chronicle::query::plan::StructuredPlan::try_from(
        crate::chronicle::query::planner::parse(
            r#"{"operation":"list","note_type":"character","filters":{"conditions":[{"field":"life_status_cause","operator":"equals","value":"Great Dungeon Fight"}]}}"#,
        )?,
    )?;
    db.resolve_string_or_wikilinks(&mut plain_target_plan, AccessScope::Gm)
        .await?;
    assert_eq!(
        plain_target_plan
            .selection()
            .ok_or_else(|| anyhow::anyhow!("missing selection"))?
            .1
            .conditions[0]
            .value,
        "[[Great Dungeon Fight]]"
    );
    assert_eq!(
        db.execute_plan_for(&plain_target_plan, AccessScope::Gm)
            .await?
            .total,
        1
    );

    let mut literal_plan = crate::chronicle::query::plan::StructuredPlan::try_from(
        crate::chronicle::query::planner::parse(
            r#"{"operation":"list","note_type":"character","filters":{"conditions":[{"field":"life_status_cause","operator":"equals","value":"old age"}]}}"#,
        )?,
    )?;
    db.resolve_string_or_wikilinks(&mut literal_plan, AccessScope::Gm)
        .await?;
    assert_eq!(
        literal_plan
            .selection()
            .ok_or_else(|| anyhow::anyhow!("missing selection"))?
            .1
            .conditions[0]
            .value,
        "old age"
    );
    Ok(())
}

#[tokio::test]
async fn string_or_wikilink_resolution_respects_field_type_and_access_scope() -> Result<()> {
    let (_directory, db) = test_database().await?;
    for (path, source) in [
        (
            "field-context.md",
            "---\nid: field-context\ntype: character\nstatus: canon\nvisibility: player\ncreated: 2026-09-07\nupdated: 2026-09-07\nlocation: '[[Field Candidate]]'\n---\n",
        ),
        (
            "type-context.md",
            "---\nid: type-context\ntype: deity\nstatus: canon\nvisibility: player\ncreated: 2026-09-07\nupdated: 2026-09-07\ndomain: '[[Type Candidate]]'\n---\n",
        ),
        (
            "access-context.md",
            "---\nid: access-context\ntype: character\nstatus: canon\nvisibility: secret\ncreated: 2026-09-07\nupdated: 2026-09-07\nlife_status_cause: '[[Hidden Candidate]]'\n---\n",
        ),
    ] {
        let (metadata, _) =
            crate::chronicle::indexer::frontmatter::parse(source)?.context("note")?;
        db.replace_note(path, "hash", &[], &[], &metadata).await?;
    }

    let make_plan = |value: &str| -> Result<crate::chronicle::query::plan::StructuredPlan> {
        crate::chronicle::query::plan::StructuredPlan::try_from(
            crate::chronicle::query::planner::parse(&format!(
                r#"{{"operation":"list","note_type":"character","filters":{{"conditions":[{{"field":"life_status_cause","operator":"equals","value":"{value}"}}]}}}}"#
            ))?,
        )
    };

    let mut field_plan = make_plan("Field Candidate")?;
    db.resolve_string_or_wikilinks(&mut field_plan, AccessScope::Gm)
        .await?;
    let field_value = &field_plan
        .selection()
        .context("field-context plan has no selection")?
        .1
        .conditions[0]
        .value;
    assert_eq!(field_value, "Field Candidate");

    let mut type_plan = make_plan("Type Candidate")?;
    db.resolve_string_or_wikilinks(&mut type_plan, AccessScope::Gm)
        .await?;
    let type_value = &type_plan
        .selection()
        .context("type-context plan has no selection")?
        .1
        .conditions[0]
        .value;
    assert_eq!(type_value, "Type Candidate");

    let mut player_plan = make_plan("Hidden Candidate")?;
    db.resolve_string_or_wikilinks(&mut player_plan, AccessScope::Player)
        .await?;
    let player_value = &player_plan
        .selection()
        .context("player-context plan has no selection")?
        .1
        .conditions[0]
        .value;
    assert_eq!(player_value, "Hidden Candidate");

    let mut gm_plan = make_plan("Hidden Candidate")?;
    db.resolve_string_or_wikilinks(&mut gm_plan, AccessScope::Gm)
        .await?;
    let gm_value = &gm_plan
        .selection()
        .context("GM-context plan has no selection")?
        .1
        .conditions[0]
        .value;
    assert_eq!(gm_value, "[[Hidden Candidate]]");
    Ok(())
}

#[tokio::test]
async fn count_members_resolves_note_identifiers_and_deduplicates_values() -> Result<()> {
    let (_directory, db) = test_database().await?;
    let source = "---\nid: ada\ntype: character\nstatus: canon\nvisibility: player\ncreated: 2026-09-07\nupdated: 2026-09-07\naliases: [The Gardener]\nenemies: ['[[Bela]]', '[[Bela]]', '[[Corin]]']\n---\n";
    let (metadata, _) = crate::chronicle::indexer::frontmatter::parse(source)?.context("note")?;
    db.replace_note("Ada.md", "hash", &[], &[], &metadata)
        .await?;

    for subject in ["[[Ada]]", "[[ada]]", "[[The Gardener]]"] {
        let plan = crate::chronicle::query::plan::StructuredPlan::try_from(
            crate::chronicle::query::planner::parse(&format!(
                r#"{{"operation":"count_members","note_type":"character","subject":"{subject}","field":"enemies"}}"#,
            ))?,
        )?;
        assert_eq!(
            db.execute_plan_for(&plan, AccessScope::Gm).await?.total,
            2,
            "{subject}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn player_structured_queries_exclude_secret_notes() -> Result<()> {
    let (_directory, db) = test_database().await?;
    for (id, visibility) in [("public-npc", "player"), ("secret-npc", "secret")] {
        let source = format!(
            "---\nid: {id}\ntype: character\nstatus: canon\nvisibility: {visibility}\ncreated: 2026-09-07\nupdated: 2026-09-07\nrole: npc\n---\n"
        );
        let (metadata, _) =
            crate::chronicle::indexer::frontmatter::parse(&source)?.context("note")?;
        db.replace_note(&format!("{id}.md"), id, &[], &[], &metadata)
            .await?;
    }
    let plan = crate::chronicle::query::plan::StructuredPlan::try_from(
        crate::chronicle::query::planner::parse(
            r#"{"operation":"count","note_type":"character","filters":{"conditions":[{"field":"role","operator":"equals","value":"npc"}]}}"#,
        )?,
    )?;
    assert_eq!(
        db.execute_plan_for(&plan, AccessScope::Player).await?.total,
        1
    );
    assert_eq!(db.execute_plan_for(&plan, AccessScope::Gm).await?.total, 2);
    Ok(())
}

#[tokio::test]
async fn replacing_a_note_type_removes_the_old_type_metadata() -> Result<()> {
    let (_directory, db) = test_database().await?;
    let character = "---\nid: shifting-note\ntype: character\nstatus: canon\nvisibility: player\ncreated: 2026-09-07\nupdated: 2026-09-07\nrole: npc\nlife_status: alive\nlocation: '[[Northmere]]'\n---\n";
    let (metadata, _) =
        crate::chronicle::indexer::frontmatter::parse(character)?.context("character")?;
    let document_id = db
        .replace_note("Shifting.md", "character", &[], &[], &metadata)
        .await?;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM character_metadata WHERE document_id = ?"
        )
        .bind(document_id)
        .fetch_one(&db.pool)
        .await?,
        1
    );

    let organisation = character
        .replace("type: character", "type: organisation")
        .replace(
            "role: npc\nlife_status: alive\nlocation: '[[Northmere]]'",
            "organisation_type: guild\npatron_deities: ['[[Aurelia]]']",
        );
    let (metadata, _) =
        crate::chronicle::indexer::frontmatter::parse(&organisation)?.context("organisation")?;
    db.replace_note("Shifting.md", "organisation", &[], &[], &metadata)
        .await?;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM character_metadata WHERE document_id = ?"
        )
        .bind(document_id)
        .fetch_one(&db.pool)
        .await?,
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM organisation_metadata WHERE document_id = ?"
        )
        .bind(document_id)
        .fetch_one(&db.pool)
        .await?,
        1
    );
    assert_eq!(sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM note_wikilinks WHERE document_id = ? AND field_name = 'patron_deities'").bind(document_id).fetch_one(&db.pool).await?, 1);
    Ok(())
}

#[tokio::test]
async fn lexical_index_tracks_replacements_deletions_and_reopen() -> Result<()> {
    let (directory, database) = test_database().await?;
    let id = database
        .replace_document(
            "guide.md",
            "a",
            &chunks(),
            &[embedding(0.0), embedding(1.0)],
        )
        .await?;
    assert_eq!(
        database
            .search_lexical_for("First", 10, AccessScope::Gm)
            .await?
            .len(),
        1
    );
    assert_eq!(
        database
            .search_lexical_for("Introduction", 10, AccessScope::Gm)
            .await?
            .len(),
        2
    );
    assert!(
        database
            .search_lexical_for("\" * : ()", 10, AccessScope::Gm)
            .await?
            .is_empty()
    );
    let replacement = vec![IndexedChunk {
        text: "Moonspire sanctuary".into(),
        ..chunks().remove(0)
    }];
    database
        .replace_document("guide.md", "b", &replacement, &[embedding(0.0)])
        .await?;
    assert!(
        database
            .search_lexical_for("First", 10, AccessScope::Gm)
            .await?
            .is_empty()
    );
    assert_eq!(
        database
            .search_lexical_for("Where is Moonspire?", 10, AccessScope::Gm)
            .await?
            .len(),
        1
    );
    let reopened = IndexerDb::open(&format!(
        "sqlite://{}",
        directory.path().join("chronicle.db").display()
    ))
    .await?;
    assert_eq!(
        reopened
            .search_lexical_for("Moonspire", 10, AccessScope::Gm)
            .await?
            .len(),
        1
    );
    database.delete_document(id).await?;
    assert!(
        reopened
            .search_lexical_for("Moonspire", 10, AccessScope::Gm)
            .await?
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn player_search_excludes_secret_chunks_while_gm_search_includes_them() -> Result<()> {
    let (_directory, database) = test_database().await?;
    let chunks = [IndexedChunk {
        chunk_index: 0,
        heading: Some("GM notes".into()),
        text: "moon-key-needle is hidden below the altar".into(),
        visibility: crate::chronicle::indexer::document::ChunkVisibility::Secret,
        overlaps_previous: false,
    }];
    database
        .replace_note(
            "mixed.md",
            "visibility-hash",
            &chunks,
            &[embedding(0.0)],
            &crate::chronicle::indexer::frontmatter::Metadata::default(),
        )
        .await?;

    assert!(
        database
            .search_lexical_for("moon-key-needle", 5, AccessScope::Player)
            .await?
            .is_empty()
    );
    assert_eq!(
        database
            .search_lexical_for("moon-key-needle", 5, AccessScope::Gm)
            .await?
            .len(),
        1
    );
    assert!(
        database
            .search_similar_for(&embedding(0.0), 5, AccessScope::Player)
            .await?
            .is_empty()
    );
    assert_eq!(
        database
            .search_similar_for(&embedding(0.0), 5, AccessScope::Gm)
            .await?
            .len(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn replace_document_rejects_mismatched_inputs_without_writing() -> anyhow::Result<()> {
    let (_directory, database) = test_database().await?;

    let Err(error) = database
        .replace_document("guide.md", "hash", &chunks(), &[embedding(0.0)])
        .await
    else {
        anyhow::bail!("mismatched chunks and embeddings should fail");
    };

    assert!(error.to_string().contains("Chunk/embedding count mismatch"));
    assert!(database.all_documents().await?.is_empty());
    assert!(!database.has_chunks().await?);
    Ok(())
}

#[tokio::test]
async fn replacement_keeps_document_identity_and_removes_stale_chunks() -> anyhow::Result<()> {
    let (_directory, database) = test_database().await?;
    let document_id = database
        .replace_document(
            "guide.md",
            "first-hash",
            &chunks(),
            &[embedding(0.0), embedding(1.0)],
        )
        .await?;

    let replacement = vec![IndexedChunk {
        chunk_index: 0,
        heading: None,
        text: "Replacement chunk".into(),
        visibility: crate::chronicle::indexer::document::ChunkVisibility::Player,
        overlaps_previous: false,
    }];
    let replacement_id = database
        .replace_document("guide.md", "second-hash", &replacement, &[embedding(2.0)])
        .await?;

    assert_eq!(replacement_id, document_id);
    assert_eq!(
        database
            .all_documents()
            .await?
            .into_iter()
            .map(|document| (document.path, document.content_hash))
            .collect::<Vec<_>>(),
        vec![("guide.md".into(), "second-hash".into())]
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM chunks")
            .fetch_one(&database.pool)
            .await?,
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT text FROM chunks")
            .fetch_one(&database.pool)
            .await?,
        "Replacement chunk"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM chunk_embeddings_player")
            .fetch_one(&database.pool)
            .await?,
        1
    );
    Ok(())
}

#[tokio::test]
async fn delete_document_removes_chunks_embeddings_and_corpus_state() -> anyhow::Result<()> {
    let (_directory, database) = test_database().await?;
    let document_id = database
        .replace_document(
            "guide.md",
            "hash",
            &chunks(),
            &[embedding(0.0), embedding(1.0)],
        )
        .await?;
    assert!(database.has_chunks().await?);

    database.delete_document(document_id).await?;

    assert!(database.all_documents().await?.is_empty());
    assert!(!database.has_chunks().await?);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM chunk_embeddings_player")
            .fetch_one(&database.pool)
            .await?,
        0
    );
    Ok(())
}

#[tokio::test]
async fn search_rejects_wrong_dimension_and_short_circuits_zero_limit() -> anyhow::Result<()> {
    let (_directory, database) = test_database().await?;

    assert!(
        database
            .search_similar_for(&[0.0], 1, AccessScope::Gm)
            .await
            .is_err()
    );
    assert!(
        database
            .search_similar_for(&embedding(0.0), 0, AccessScope::Gm)
            .await?
            .is_empty()
    );
    Ok(())
}
