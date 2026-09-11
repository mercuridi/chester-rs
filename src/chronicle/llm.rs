use std::{
    fs::File,
    io::BufReader,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result, anyhow, bail};
use candle_core::{Device, Tensor, quantized::gguf_file};
use candle_transformers::{generation::LogitsProcessor, models::quantized_qwen2::ModelWeights};
use hf_hub::{Repo, RepoType, api::sync::Api};
use tokenizers::Tokenizer;

use super::{
    config::chronicle::LlmSettings,
    query::plan::RouteOperation,
    runtime::{GpuRuntime, report_cuda_oom},
};
use tracing::{info, instrument};

#[async_trait::async_trait]
pub trait LanguageModel: Send + Sync {
    fn prompt_token_budget(&self) -> usize;
    fn count_input_tokens(&self, prompt: &str) -> Result<usize>;
    async fn generate(&self, prompt: &str) -> Result<String>;
    async fn classify_route(&self, question: &str) -> Result<String>;
    async fn generate_structured_plan(
        &self,
        question: &str,
        operation: RouteOperation,
    ) -> Result<String>;
    async fn repair_structured_plan(
        &self,
        question: &str,
        operation: RouteOperation,
        _rejected_response: &str,
        _rejection_error: &str,
    ) -> Result<String> {
        self.generate_structured_plan(question, operation).await
    }
    async fn load(&self) -> Result<()>;
    async fn unload(&self) -> Result<()>;
}

#[derive(Clone)]
pub struct Llm {
    model: Arc<Mutex<Option<LoadedLlm>>>,
    runtime: GpuRuntime,
    repo: String,
    revision: String,
    model_file: String,
    tokenizer_repo: String,
    tokenizer_file: String,
    max_tokens: usize,
    context_limit: usize,
    temperature: f64,
    seed: u64,
    system_prompt: String,
}

struct LoadedLlm {
    model: ModelWeights,
    tokenizer: Tokenizer,
    device: Device,
    eos_tokens: Vec<u32>,
}

impl Llm {
    pub fn new(config: &LlmSettings, runtime: GpuRuntime) -> Self {
        Self {
            model: Arc::new(Mutex::new(None)),
            runtime,
            repo: config.model.repo.clone(),
            revision: config.model.revision.clone(),
            model_file: config.model.file.clone(),
            tokenizer_repo: config.tokenizer.repo.clone(),
            tokenizer_file: config.tokenizer.file.clone(),
            max_tokens: config.generation.max_tokens as usize,
            context_limit: config.generation.context_limit,
            temperature: f64::from(config.generation.temperature),
            seed: config.generation.seed,
            system_prompt: format!(
                "{}\n\nKeep every answer at or below {} characters.",
                config.generation.system_prompt.trim_end(),
                config.generation.max_reply_length
            ),
        }
    }

    #[instrument(skip(self))]
    pub async fn load(&self) -> Result<()> {
        info!(repo = %self.repo, revision = %self.revision, "Loading Chronicle LLM");
        let lease = self.runtime.begin_llm_load()?;
        let model = Arc::clone(&self.model);
        let repo = self.repo.clone();
        let revision = self.revision.clone();
        let model_file = self.model_file.clone();
        let tokenizer_repo = self.tokenizer_repo.clone();
        let tokenizer_file = self.tokenizer_file.clone();

        let result = tokio::task::spawn_blocking(move || {
            let loaded = LoadedLlm::load(
                &repo,
                &revision,
                &model_file,
                &tokenizer_repo,
                &tokenizer_file,
            )?;

            let mut model = model
                .lock()
                .map_err(|_| anyhow!("LLM model state is poisoned"))?;

            if model.is_some() {
                bail!("The LLM model was loaded concurrently");
            }

            *model = Some(loaded);
            drop(model);

            // The worker owns the lease through both model construction and
            // the successful runtime state transition.
            lease.commit_to_loaded()
        })
        .await
        .context("Native LLM loading task failed")?;

        if let Err(error) = &result {
            report_cuda_oom(error, "llm", "load");
        }

        info!("Chronicle LLM loaded");

        result
    }

    #[instrument(skip(self))]
    pub async fn unload(&self) -> Result<()> {
        info!("Unloading Chronicle LLM");
        let lease = self.runtime.begin_llm_unload()?;

        let model = {
            let mut model_slot = self
                .model
                .lock()
                .map_err(|_| anyhow!("LLM model state is poisoned"))?;
            model_slot.take()
        };

        tokio::task::spawn_blocking(move || {
            let _model = model;
            lease.commit_to_idle()
        })
        .await
        .context("Native LLM unload task failed")??;
        info!("Chronicle LLM unloaded");
        Ok(())
    }

    pub fn prompt_token_budget(&self) -> usize {
        self.context_limit - self.max_tokens
    }

    pub fn count_input_tokens(&self, prompt: &str) -> Result<usize> {
        let model = self
            .model
            .lock()
            .map_err(|_| anyhow!("LLM model state is poisoned"))?;
        let loaded = model
            .as_ref()
            .ok_or_else(|| anyhow!("Chronicle LLM is not loaded; run /chronicle start first"))?;
        let user_prompt = self.format_input_prompt(prompt);
        let encoded = loaded
            .tokenizer
            .encode(user_prompt, true)
            .map_err(|error| anyhow!("Failed to tokenize LLM prompt: {error}"))?;
        Ok(encoded.len())
    }

    #[instrument(skip(self, prompt), fields(prompt_len = prompt.len()))]
    pub async fn generate(&self, prompt: &str) -> Result<String> {
        self.generate_with_system(
            &self.system_prompt,
            prompt,
            self.max_tokens,
            self.temperature,
        )
        .await
    }

    pub async fn classify_route(&self, question: &str) -> Result<String> {
        self.generate_with_system(
            crate::chronicle::query::classifier::system_prompt(),
            question,
            32,
            0.0,
        )
        .await
    }

    pub async fn generate_structured_plan(
        &self,
        question: &str,
        operation: RouteOperation,
    ) -> Result<String> {
        let system = crate::chronicle::query::planner::structured_system_prompt(operation);
        self.generate_with_system(&system, question, 256, 0.0).await
    }

    pub async fn repair_structured_plan(
        &self,
        question: &str,
        operation: RouteOperation,
        rejected_response: &str,
        rejection_error: &str,
    ) -> Result<String> {
        let system = crate::chronicle::query::planner::structured_system_prompt(operation);
        let guidance = plan_repair_guidance(rejection_error);
        let prompt = format!(
            "Original question:\n{question}\n\nYour previous response was rejected:\n<rejected-plan>\n{rejected_response}\n</rejected-plan>\n\nValidation error:\n<validation-error>\n{rejection_error}\n</validation-error>\n\n{guidance}\n\nCorrect the specific validation error. Return a corrected query plan for the original question. Output exactly one JSON object and nothing else."
        );
        self.generate_with_system(&system, &prompt, 256, 0.0).await
    }

    async fn generate_with_system(
        &self,
        system: &str,
        prompt: &str,
        max_tokens: usize,
        temperature: f64,
    ) -> Result<String> {
        let model = Arc::clone(&self.model);
        let user_prompt = format_chat_prompt(system, prompt);
        let context_limit = self.context_limit;
        let seed = self.seed;
        let gpu_lease = self.runtime.acquire_inference()?;

        let result = tokio::task::spawn_blocking(move || {
            // Keep inference exclusive until the native worker has completely
            // finished, even if the async caller is cancelled.
            let _gpu_lease = gpu_lease;
            let mut model = model
                .lock()
                .map_err(|_| anyhow!("LLM model state is poisoned"))?;
            let loaded = model.as_mut().ok_or_else(|| {
                anyhow!("Chronicle LLM is not loaded; run /chronicle start first")
            })?;

            loaded.model.clear_kv_cache();
            let encoded = loaded
                .tokenizer
                .encode(user_prompt, true)
                .map_err(|error| anyhow!("Failed to tokenize LLM prompt: {error}"))?;
            let prompt_tokens = encoded.get_ids();

            if prompt_tokens.is_empty() {
                bail!("LLM tokenizer produced an empty prompt");
            }
            if prompt_tokens.len().saturating_add(max_tokens) > context_limit {
                bail!(
                    "LLM prompt and generation budget exceed context limit: {} + {} > {}",
                    prompt_tokens.len(),
                    max_tokens,
                    context_limit
                );
            }

            // Clear the cache even if a CUDA allocation or kernel call fails.
            // Without this, a failed long request can poison every later request
            // by retaining its partial KV cache on the GPU.
            let result = (|| -> Result<(String, usize)> {
                let mut logits_processor = LogitsProcessor::new(seed, Some(temperature), None);
                let input = Tensor::new(prompt_tokens, &loaded.device)?.unsqueeze(0)?;
                let logits = loaded.model.forward(&input, 0)?.squeeze(0)?;
                let mut next_token = logits_processor.sample(&logits)?;
                let mut generated = Vec::with_capacity(max_tokens);

                for index in 0..max_tokens {
                    if loaded.eos_tokens.contains(&next_token) {
                        break;
                    }

                    generated.push(next_token);

                    if index + 1 == max_tokens {
                        break;
                    }

                    let input = Tensor::new(&[next_token], &loaded.device)?.unsqueeze(0)?;
                    let logits = loaded
                        .model
                        .forward(&input, prompt_tokens.len() + index)?
                        .squeeze(0)?;
                    next_token = logits_processor.sample(&logits)?;
                }

                let response = loaded
                    .tokenizer
                    .decode(&generated, true)
                    .map_err(|error| anyhow!("Failed to decode LLM response: {error}"))?;
                Ok((response, generated.len()))
            })();
            loaded.model.clear_kv_cache();
            let (response, generated_tokens) = result?;

            let response = response.trim().to_owned();
            tracing::debug!(response = %response, "LLM inference result");
            tracing::info!(
                response_len = response.len(),
                generated_tokens,
                "LLM inference complete"
            );
            Ok(response)
        })
        .await
        .context("Native LLM inference task failed")?;

        if let Err(error) = &result {
            report_cuda_oom(error, "llm", "inference");
        }
        result
    }

    fn format_input_prompt(&self, prompt: &str) -> String {
        format_chat_prompt(&self.system_prompt, prompt)
    }
}

fn format_chat_prompt(system: &str, prompt: &str) -> String {
    format!(
        "<|im_start|>system\n{system}<|im_end|>\n<|im_start|>user\n{prompt}<|im_end|>\n<|im_start|>assistant\n"
    )
}

#[async_trait::async_trait]
impl LanguageModel for Llm {
    fn prompt_token_budget(&self) -> usize {
        self.prompt_token_budget()
    }
    fn count_input_tokens(&self, prompt: &str) -> Result<usize> {
        self.count_input_tokens(prompt)
    }
    async fn generate(&self, prompt: &str) -> Result<String> {
        self.generate(prompt).await
    }
    async fn classify_route(&self, question: &str) -> Result<String> {
        self.classify_route(question).await
    }
    async fn generate_structured_plan(
        &self,
        question: &str,
        operation: RouteOperation,
    ) -> Result<String> {
        self.generate_structured_plan(question, operation).await
    }
    async fn repair_structured_plan(
        &self,
        question: &str,
        operation: RouteOperation,
        rejected_response: &str,
        rejection_error: &str,
    ) -> Result<String> {
        self.repair_structured_plan(question, operation, rejected_response, rejection_error)
            .await
    }
    async fn load(&self) -> Result<()> {
        self.load().await
    }
    async fn unload(&self) -> Result<()> {
        self.unload().await
    }
}

impl LoadedLlm {
    fn load(
        repo_name: &str,
        revision: &str,
        model_file: &str,
        tokenizer_repo_name: &str,
        tokenizer_file: &str,
    ) -> Result<Self> {
        let api = Api::new().context("Failed to initialize Hugging Face Hub")?;
        let model_repo = api.repo(Repo::with_revision(
            repo_name.to_owned(),
            RepoType::Model,
            revision.to_owned(),
        ));
        let tokenizer_repo = api.repo(Repo::with_revision(
            tokenizer_repo_name.to_owned(),
            RepoType::Model,
            revision.to_owned(),
        ));

        let model_path = model_repo
            .get(model_file)
            .with_context(|| format!("Failed to download/load LLM model file `{model_file}`"))?;
        let tokenizer_path = tokenizer_repo
            .get(tokenizer_file)
            .with_context(|| format!("Failed to download/load LLM tokenizer `{tokenizer_file}`"))?;

        let device = Device::cuda_if_available(0)
            .context("Failed to initialize CUDA device 0 for the LLM")?;
        let mut model_file = BufReader::new(
            File::open(&model_path)
                .with_context(|| format!("Failed to open LLM model `{}`", model_path.display()))?,
        );
        let content = gguf_file::Content::read(&mut model_file)
            .context("Failed to parse LLM GGUF metadata")?;
        let model = ModelWeights::from_gguf(content, &mut model_file, &device)
            .context("Failed to construct the quantized Qwen2 model")?;
        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|error| anyhow!("Failed to load LLM tokenizer: {error}"))?;

        let eos_tokens = ["<|im_end|>", "<|endoftext|>"]
            .into_iter()
            .filter_map(|token| tokenizer.token_to_id(token))
            .collect();

        Ok(Self {
            model,
            tokenizer,
            device,
            eos_tokens,
        })
    }
}

fn plan_repair_guidance(rejection_error: &str) -> &'static str {
    if rejection_error.contains("Invalid wikilink query value") {
        "This is a wikilink-format failure. Preserve the target, but write every affected wikilink value exactly as [[Target]]; do not add a prefix such as `contains:`."
    } else {
        "Correct only the validation error shown above."
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, clippy::unwrap_used)]
mod tests {
    use super::Llm;
    use crate::chronicle::{
        config::chronicle::{GenerationSettings, LlmSettings, ModelSource, TokenizerSource},
        runtime::GpuRuntime,
    };

    fn config() -> LlmSettings {
        LlmSettings {
            model: ModelSource {
                repo: "repo".into(),
                revision: "revision".into(),
                file: "model".into(),
            },
            tokenizer: TokenizerSource {
                repo: "tokenizer-repo".into(),
                file: "tokenizer".into(),
            },
            generation: GenerationSettings {
                max_tokens: 256,
                context_limit: 1024,
                temperature: 0.5,
                seed: 7,
                system_prompt: "System prompt\n\n".into(),
                max_reply_length: 100,
            },
        }
    }

    #[test]
    fn constructor_derives_prompt_budget_and_reply_instruction() {
        let llm = Llm::new(&config(), GpuRuntime::new());
        assert_eq!(llm.prompt_token_budget(), 768);
        assert_eq!(llm.max_tokens, 256);
        assert_eq!(llm.temperature, 0.5);
        assert_eq!(llm.seed, 7);
        assert_eq!(
            llm.system_prompt,
            "System prompt\n\nKeep every answer at or below 100 characters."
        );
    }

    #[test]
    fn input_prompt_uses_qwen_chat_markers() {
        let llm = Llm::new(&config(), GpuRuntime::new());
        assert_eq!(
            llm.format_input_prompt("Question"),
            "<|im_start|>system\nSystem prompt\n\nKeep every answer at or below 100 characters.<|im_end|>\n<|im_start|>user\nQuestion<|im_end|>\n<|im_start|>assistant\n"
        );
    }

    #[test]
    fn repair_guidance_is_specific_for_wikilink_failures() {
        assert!(super::plan_repair_guidance("Invalid wikilink query value").contains("[[Target]]"));
        assert_eq!(
            super::plan_repair_guidance("unknown field `x`"),
            "Correct only the validation error shown above."
        );
    }

    #[test]
    fn token_count_requires_loaded_model() {
        let llm = Llm::new(&config(), GpuRuntime::new());
        let error = llm.count_input_tokens("Question").unwrap_err();
        assert!(error.to_string().contains("not loaded"));
    }

    #[tokio::test]
    async fn generation_requires_loaded_model() {
        let llm = Llm::new(&config(), GpuRuntime::new());
        let error = llm.generate("Question").await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unavailable until the LLM is loaded")
        );
    }
}
