use crate::chronicle::indexer::db::repository::SearchResult;

pub fn build_prompt(question: &str, results: &[SearchResult]) -> String {
    let mut prompt = String::new();

    prompt.push_str(
        "Answer the question using only the provided Chronicle context. \
         If the context does not contain enough information, say so.\n\n",
    );

    prompt.push_str("Context:\n");

    for (index, result) in results.iter().enumerate() {
        prompt.push_str(&format!("[{}] {}", index + 1, result.document_path,));

        if let Some(heading) = &result.heading {
            prompt.push_str(&format!(" — {heading}"));
        }

        prompt.push_str("\n");
        prompt.push_str(&result.text);
        prompt.push_str("\n\n");
    }

    prompt.push_str("Question:\n");
    prompt.push_str(question);
    prompt.push_str("\n\nAnswer:");

    prompt
}
