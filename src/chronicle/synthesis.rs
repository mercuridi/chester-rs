use anyhow::{Result, bail};
use std::fmt::Write as _;

use super::indexer::db::repository::SearchResult;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceNote {
    pub source_labels: Vec<String>,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct BatchPlan {
    pub batches: Vec<Vec<EvidenceNote>>,
    pub omitted_items: usize,
}

pub fn retrieved_evidence(results: &[SearchResult]) -> Vec<EvidenceNote> {
    results
        .iter()
        .enumerate()
        .map(|(index, result)| {
            let label = format!("S{}", index + 1);
            let document = std::path::Path::new(&result.document_path)
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy();
            let mut text = format!("Document: {document}\n");
            if let Some(heading) = &result.heading {
                let _ = writeln!(text, "Heading: {heading}");
            }
            text.push_str("Content:\n");
            text.push_str(&result.text);
            EvidenceNote {
                source_labels: vec![label],
                text,
            }
        })
        .collect()
}

pub fn pack_batches<F>(
    question: &str,
    items: &[EvidenceNote],
    token_budget: usize,
    max_batches: usize,
    prompt: fn(&str, &[EvidenceNote]) -> String,
    token_count: F,
) -> Result<BatchPlan>
where
    F: Fn(&str) -> Result<usize>,
{
    if max_batches == 0 {
        bail!("Synthesis max_batches must be positive");
    }

    let mut batches = Vec::new();
    let mut current = Vec::new();
    let mut omitted_items = 0;

    for item in items {
        let mut candidate = current.clone();
        candidate.push(item.clone());
        if token_count(&prompt(question, &candidate))? <= token_budget {
            current = candidate;
            continue;
        }

        if current.is_empty() {
            bail!("A single synthesis evidence item exceeds the configured batch token budget");
        }
        batches.push(current);
        if batches.len() == max_batches {
            omitted_items = items
                .len()
                .saturating_sub(batches.iter().map(Vec::len).sum());
            return Ok(BatchPlan {
                batches,
                omitted_items,
            });
        }
        current = vec![item.clone()];
        if token_count(&prompt(question, &current))? > token_budget {
            bail!("A single synthesis evidence item exceeds the configured batch token budget");
        }
    }

    if !current.is_empty() {
        batches.push(current);
    }
    Ok(BatchPlan {
        batches,
        omitted_items,
    })
}

pub fn map_prompt(question: &str, sources: &[EvidenceNote]) -> String {
    let mut prompt = String::from(
        "Extract compact, faithful evidence notes relevant to the question from the supplied passages. \
         Do not answer the user yet. Preserve chronology, named entities, direct relationships, \
         uncertainty, contradictions, and missing coverage. Do not invent causal connections. \
         The source labels are internal provenance; retain them in your notes.\n\n<evidence>\n",
    );
    write_items(&mut prompt, sources, "source");
    prompt.push_str("</evidence>\n\nQuestion:\n");
    prompt.push_str(question);
    prompt.push_str("\n\nEvidence notes:");
    prompt
}

pub fn reduce_prompt(question: &str, notes: &[EvidenceNote]) -> String {
    let mut prompt = String::from(
        "Merge these intermediate evidence notes into a shorter, faithful evidence note for a later \
         narrative answer. Preserve chronology, named entities, contradictions, uncertainty, missing \
         coverage, and the internal source labels. Do not answer the user and do not invent facts.\n\n<evidence_notes>\n",
    );
    write_items(&mut prompt, notes, "note");
    prompt.push_str("</evidence_notes>\n\nQuestion:\n");
    prompt.push_str(question);
    prompt.push_str("\n\nReduced evidence note:");
    prompt
}

pub fn final_prompt(question: &str, notes: &[EvidenceNote]) -> String {
    let mut prompt = String::from(
        "Answer the question as a coherent, concise narrative using only these evidence notes. \
         Use chronology when supported. Clearly distinguish documented facts from cautious interpretation. \
         Do not cite sources, mention source labels, or add a sources-consulted section. Do not invent \
         dates, motives, causal links, or completeness, and never claim exhaustive coverage unless the \
         evidence notes establish it. Mention uncertainty, conflicting accounts, or incomplete coverage \
         only when the evidence notes show a material gap or conflict.\n\n<evidence_notes>\n",
    );
    write_items(&mut prompt, notes, "note");
    prompt.push_str("</evidence_notes>\n\nQuestion:\n");
    prompt.push_str(question);
    prompt.push_str("\n\nAnswer:");
    prompt
}

fn write_items(prompt: &mut String, items: &[EvidenceNote], tag: &str) {
    for item in items {
        let labels = item.source_labels.join(",");
        let _ = writeln!(prompt, "<{tag} sources=\"{labels}\">");
        prompt.push_str(&item.text);
        let _ = writeln!(prompt, "\n</{tag}>");
    }
}

pub fn merged_labels(notes: &[EvidenceNote]) -> Vec<String> {
    let mut labels = Vec::new();
    for note in notes {
        for label in &note.source_labels {
            if !labels.contains(label) {
                labels.push(label.clone());
            }
        }
    }
    labels
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(label: &str, text: &str) -> EvidenceNote {
        EvidenceNote {
            source_labels: vec![label.into()],
            text: text.into(),
        }
    }

    #[test]
    fn packs_ranked_items_and_caps_batch_count() -> Result<()> {
        let items = [note("S1", "aaaa"), note("S2", "bbbb"), note("S3", "cccc")];
        let budget = map_prompt("q", &items[..2]).len();
        let plan = pack_batches(
            "q",
            &items,
            budget,
            1,
            map_prompt,
            |prompt| Ok(prompt.len()),
        )?;
        assert_eq!(plan.batches.len(), 1);
        assert_eq!(plan.batches[0], vec![items[0].clone(), items[1].clone()]);
        assert_eq!(plan.omitted_items, 1);
        Ok(())
    }

    #[test]
    fn rejects_item_that_cannot_fit_alone() {
        let error = pack_batches(
            "q",
            &[note("S1", "x".repeat(100).as_str())],
            10,
            1,
            map_prompt,
            |prompt| Ok(prompt.len()),
        )
        .unwrap_err();
        assert!(error.to_string().contains("single synthesis evidence item"));
    }

    #[test]
    fn final_prompt_sets_narrative_and_evidence_boundaries() {
        let prompt = final_prompt("What happened?", &[note("S1", "A battle occurred.")]);
        assert!(prompt.contains("coherent, concise narrative"));
        assert!(prompt.contains("Use chronology when supported."));
        assert!(prompt.contains("distinguish documented facts from cautious interpretation"));
        assert!(prompt.contains("Do not cite sources, mention source labels"));
        assert!(prompt.contains("never claim exhaustive coverage"));
        assert!(prompt.contains("sources=\"S1\""));
    }
}
