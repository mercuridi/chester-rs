// Structured metadata count and list queries.
use anyhow::{Context, Result};
use sqlx::{QueryBuilder, Row, Sqlite};

use super::facade::{AccessScope, IndexerDb, StructuredNote, StructuredResult};

impl IndexerDb {
    pub async fn execute_plan_for(
        &self,
        plan: &crate::chronicle::query::plan::Plan,
        access: AccessScope,
    ) -> Result<StructuredResult> {
        use crate::chronicle::query::{plan::Plan, render::LIST_LIMIT};
        let mut plan = plan.clone();
        plan.validate()?;
        if let Plan::CountMembers {
            note_type,
            subject,
            field,
        } = &plan
        {
            return self.count_members(note_type, subject, field, access).await;
        }
        self.resolve_string_or_wikilinks(&mut plan).await?;
        let (note_type, filters) = plan.selection().context("Plan is not a structured query")?;
        let mut count = structured_query(
            "SELECT COUNT(DISTINCT m.note_id) FROM note_metadata m",
            note_type,
            filters,
            access,
        )?;
        let total: i64 = count.build_query_scalar().fetch_one(&self.pool).await?;
        let mut notes = Vec::new();
        if matches!(plan, Plan::List { .. }) {
            let mut query = structured_query(
                "SELECT m.note_id, MIN(d.path) AS path FROM note_metadata m JOIN documents d ON d.id = m.document_id",
                note_type,
                filters,
                access,
            )?;
            query
                .push(" GROUP BY m.note_id ORDER BY m.note_id LIMIT ")
                .push_bind(i64::try_from(LIST_LIMIT)?);
            let rows = query.build().fetch_all(&self.pool).await?;
            for row in rows {
                let path: String = row.get("path");
                notes.push(StructuredNote {
                    id: row.get("note_id"),
                    title: std::path::Path::new(&path)
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                });
            }
        }
        Ok(StructuredResult { total, notes })
    }

    async fn count_members(
        &self,
        note_type: &str,
        subject: &str,
        field: &str,
        access: AccessScope,
    ) -> Result<StructuredResult> {
        let target = &subject.trim()[2..subject.trim().len() - 2];
        let mut subject_query = String::from(
            "SELECT DISTINCT m.document_id FROM note_identifiers i JOIN note_metadata m ON m.document_id = i.document_id WHERE i.value = ? COLLATE NOCASE AND m.status = 'canon' AND m.note_type = ?",
        );
        if access == AccessScope::Player {
            subject_query.push_str(" AND m.visibility != 'secret'");
        }
        let subject_ids = sqlx::query_scalar::<_, i64>(&subject_query)
            .bind(target)
            .bind(note_type)
            .fetch_all(&self.pool)
            .await?;
        anyhow::ensure!(
            subject_ids.len() == 1,
            "Member-count subject must resolve to exactly one accessible canon note"
        );
        let definition = crate::chronicle::indexer::schema::field_definition(note_type, field)
            .context("validated member-count field is missing")?;
        let table = match definition.value_type {
            crate::chronicle::indexer::schema::ValueType::WikilinkList => "note_wikilinks",
            crate::chronicle::indexer::schema::ValueType::StringList => "note_string_lists",
            _ => anyhow::bail!("validated member-count field is not a list"),
        };
        let total: i64 = sqlx::query_scalar(&format!(
            "SELECT COUNT(DISTINCT value) FROM {table} WHERE document_id = ? AND field_name = ?"
        ))
        .bind(subject_ids[0])
        .bind(field)
        .fetch_one(&self.pool)
        .await?;
        Ok(StructuredResult {
            total,
            notes: Vec::new(),
        })
    }

    /// Resolves a plain `StringOrWikilink` condition only when the exact
    /// bracketed candidate is already present in indexed metadata. Literal
    /// values remain untouched, so this cannot turn arbitrary prose into a
    /// link query.
    pub async fn resolve_string_or_wikilinks(
        &self,
        plan: &mut crate::chronicle::query::plan::Plan,
    ) -> Result<()> {
        let (note_type, filters) = match plan {
            crate::chronicle::query::plan::Plan::Count { note_type, filters }
            | crate::chronicle::query::plan::Plan::List { note_type, filters } => {
                (note_type.as_str(), filters)
            }
            _ => return Ok(()),
        };
        for condition in &mut filters.conditions {
            let Some(definition) =
                crate::chronicle::indexer::schema::field_definition(note_type, &condition.field)
            else {
                continue;
            };
            if definition.value_type
                != crate::chronicle::indexer::schema::ValueType::StringOrWikilink
                || condition.value.trim().is_empty()
                || condition.value.contains(['[', ']'])
            {
                continue;
            }
            let candidate = format!("[[{}]]", condition.value.trim());
            let exists: i64 = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM note_scalar_fields WHERE value = ? UNION ALL SELECT 1 FROM note_wikilinks WHERE value = ?)",
            )
            .bind(&candidate)
            .bind(&candidate)
            .fetch_one(&self.pool)
            .await?;
            if exists != 0 {
                condition.value = candidate;
            }
        }
        Ok(())
    }
}
fn structured_query<'a>(
    select: &str,
    note_type: &'a str,
    filters: &'a crate::chronicle::query::plan::Filters,
    access: AccessScope,
) -> Result<QueryBuilder<'a, Sqlite>> {
    use crate::chronicle::query::plan::ConditionOperator;

    let mut query = QueryBuilder::new(select);
    query
        .push(" WHERE m.status = 'canon' AND m.note_type = ")
        .push_bind(note_type);
    if access == AccessScope::Player {
        query.push(" AND m.visibility != 'secret'");
    }
    for condition in &filters.conditions {
        match condition.operator {
            ConditionOperator::Equals => {
                query
                    .push(" AND EXISTS (SELECT 1 FROM note_scalar_fields s WHERE s.document_id = m.document_id AND s.field_name = ")
                    .push_bind(&condition.field)
                    .push(" AND s.value = ")
                    .push_bind(&condition.value)
                    .push(")");
            }
            ConditionOperator::Contains => {
                let definition = crate::chronicle::indexer::schema::field_definition(
                    note_type,
                    &condition.field,
                )
                .context("validated query condition field is missing")?;
                let table = match definition.value_type {
                    crate::chronicle::indexer::schema::ValueType::WikilinkList => "note_wikilinks",
                    crate::chronicle::indexer::schema::ValueType::StringList => "note_string_lists",
                    _ => anyhow::bail!("contains condition must target a list field"),
                };
                query
                    .push(" AND EXISTS (SELECT 1 FROM ")
                    .push(table)
                    .push(" l WHERE l.document_id = m.document_id AND l.field_name = ")
                    .push_bind(&condition.field)
                    .push(" AND l.value = ")
                    .push_bind(&condition.value)
                    .push(")");
            }
        }
    }
    Ok(query)
}
