use anyhow::Result;
use tracing::info;

use super::super::{indexer::retriever::api::RetrieverApi, llm::LanguageModel};

pub(in crate::chronicle::service) async fn start(
    retriever: &dyn RetrieverApi,
    llm: &dyn LanguageModel,
) -> Result<()> {
    info!("Starting Chronicle models");
    retriever.load_embedder().await?;
    if let Err(error) = llm.load().await {
        tracing::warn!(%error, "Chronicle LLM failed to load; releasing embedder");
        retriever.unload_embedder().await?;
        return Err(error);
    }
    info!("Chronicle models ready");
    Ok(())
}

pub(in crate::chronicle::service) async fn stop(
    retriever: &dyn RetrieverApi,
    llm: &dyn LanguageModel,
) -> Result<()> {
    info!("Stopping Chronicle models");
    llm.unload().await?;
    retriever.unload_embedder().await?;
    info!("Chronicle models stopped");
    Ok(())
}
