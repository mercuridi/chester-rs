use super::plan::{Plan, StructuredPlan};
use crate::chronicle::indexer::db::StructuredResult;

pub const LIST_LIMIT: usize = 20;

pub fn render(plan: &StructuredPlan, result: &StructuredResult, max_chars: usize) -> String {
    if let Plan::CountMembers { subject, field, .. } = plan.as_plan() {
        return format!(
            "{} distinct {} recorded for {}.",
            result.total,
            field.replace('_', " "),
            subject
        )
        .chars()
        .take(max_chars)
        .collect();
    }
    let Some((note_type, _filters)) = plan.selection() else {
        return String::new();
    };
    let noun = match note_type {
        "deity" => "deity notes".to_owned(),
        "lore" | "metagame" => format!("{note_type} notes"),
        _ => format!("{note_type}s"),
    };
    let header = format!("{} canon {noun} recorded.", result.total);
    if matches!(plan.as_plan(), Plan::Count { .. }) || result.total == 0 {
        return header.chars().take(max_chars).collect();
    }
    let mut names = Vec::new();
    for note in &result.notes {
        let candidate = format!("- {} [{}]", note.title.replace(['\n', '\r'], " "), note.id);
        let mut proposed = names.clone();
        proposed.push(candidate.clone());
        let output = list_text(&header, &proposed, result.total);
        if output.chars().count() > max_chars {
            break;
        }
        names.push(candidate);
    }
    list_text(&header, &names, result.total)
        .chars()
        .take(max_chars)
        .collect()
}

fn list_text(header: &str, names: &[String], total: i64) -> String {
    let suffix = if i64::try_from(names.len()).ok() == Some(total) {
        String::new()
    } else {
        format!("\nShowing {} of {total} recorded matches.", names.len())
    };
    format!("{header}\n{}{suffix}", names.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chronicle::{indexer::db::StructuredNote, query::Filters};
    #[test]
    fn lists_report_total_and_do_not_silently_truncate_names() -> anyhow::Result<()> {
        let plan = StructuredPlan::try_from(Plan::List {
            note_type: "character".into(),
            filters: Filters::default(),
        })?;
        let result = StructuredResult {
            total: 25,
            notes: (0..20)
                .map(|i| StructuredNote {
                    id: format!("id-{i}"),
                    title: format!("Character {i}"),
                })
                .collect(),
        };
        let output = render(&plan, &result, 5000);
        assert!(output.contains("Showing 20 of 25"));
        let short = render(&plan, &result, 100);
        assert!(short.chars().count() <= 100);
        assert!(short.contains("of 25 recorded matches"));
        Ok(())
    }
}
