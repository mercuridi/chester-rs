use anyhow::Result;
use tracing::debug;

use super::super::{
    config::chronicle::{GenerationSettings, RetrievalSettings},
    indexer::{
        db::repository::facade::AccessScope,
        prompt,
        retriever::api::{RetrievalOutcome, RetrieverApi},
    },
    llm::LanguageModel,
};
use super::answer_routing::RetrievalMode;

pub(in crate::chronicle::service) async fn answer_from_retrieval(
    retriever: &dyn RetrieverApi,
    llm: &dyn LanguageModel,
    retrieval: &RetrievalSettings,
    generation: &GenerationSettings,
    question: &str,
    mode: RetrievalMode,
    access: AccessScope,
) -> Result<String> {
    let prefix = mode.prefix();
    let answer_limit = generation
        .max_reply_length
        .saturating_sub(prefix.chars().count());
    if answer_limit == 0 {
        return Ok(truncate_to_char_limit(prefix, generation.max_reply_length));
    }
    let outcome = match retriever
        .search(question, retrieval.search_settings(), access)
        .await
    {
        Ok(outcome) => outcome,
        Err(error) => {
            tracing::warn!(%error, "Chronicle retrieval failed");
            return Ok(truncate_to_char_limit(
                &format!("{prefix}Chronicle retrieval failed."),
                generation.max_reply_length,
            ));
        }
    };

    let results = match outcome {
        RetrievalOutcome::Results(results) => results,
        RetrievalOutcome::BadQuestion => {
            return Ok(truncate_to_char_limit(
                &format!("{prefix}Please provide a non-empty question."),
                generation.max_reply_length,
            ));
        }
        RetrievalOutcome::CorpusEmpty => {
            return Ok(truncate_to_char_limit(
                &format!("{prefix}Chronicle corpus is empty."),
                generation.max_reply_length,
            ));
        }
        RetrievalOutcome::NoResultMeetsThreshold => {
            return Ok(truncate_to_char_limit(
                &format!("{prefix}No relevant Chronicle context was found."),
                generation.max_reply_length,
            ));
        }
    };

    let retrieval_question = mode.retrieval_question(question);
    let assembly = prompt::build_prompt_with_budget(
        &retrieval_question,
        &results,
        llm.prompt_token_budget(),
        |candidate| llm.count_input_tokens(candidate),
    )?;
    let prompt = assembly.prompt;
    debug!(
        result_count = results.len(),
        selected_result_count = assembly.selected_results,
        omitted_result_count = assembly.omitted_results,
        prompt_tokens = assembly.prompt_tokens,
        truncated_result = assembly.truncated_result,
        prompt_len = prompt.len(),
        "Built Chronicle prompt"
    );
    let (answer, _, _) = generate_answer(llm, &prompt, answer_limit).await?;
    Ok(format!("{prefix}{answer}"))
}

pub(in crate::chronicle::service) async fn generate_answer(
    llm: &dyn LanguageModel,
    prompt: &str,
    answer_limit: usize,
) -> Result<(String, bool, bool)> {
    let mut answer = llm.generate(prompt).await?;
    let mut retried = false;
    let mut truncated = false;
    if answer.chars().count() > answer_limit {
        debug!(
            answer_len = answer.chars().count(),
            max_reply_length = answer_limit,
            "LLM answer exceeded configured length; requesting a shorter answer"
        );
        let retry_prompt = format!(
            "{prompt}\n\nThe draft answer below is too long. Rewrite it to fit within {answer_limit} characters. Preserve the most important information, and output only the shorter answer.\n\nDraft answer:\n{answer}"
        );
        answer = llm.generate(&retry_prompt).await?;
        retried = true;
    }
    if answer.chars().count() > answer_limit {
        tracing::warn!(
            answer_len = answer.chars().count(),
            max_reply_length = answer_limit,
            "LLM answer remained over length after retry; truncating"
        );
        answer = truncate_to_char_limit(&answer, answer_limit);
        truncated = true;
    }
    Ok((answer, retried, truncated))
}

pub(in crate::chronicle::service) fn truncate_to_char_limit(
    answer: &str,
    max_length: usize,
) -> String {
    answer.chars().take(max_length).collect()
}
