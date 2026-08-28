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

use super::{config::ChronicleConfig, runtime::GpuRuntime};
use tracing::{debug, info, instrument};

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
    pub fn new(config: &ChronicleConfig, runtime: GpuRuntime) -> Self {
        Self {
            model: Arc::new(Mutex::new(None)),
            runtime,
            repo: config.llm_repo.clone(),
            revision: config.llm_revision.clone(),
            model_file: config.llm_model_file.clone(),
            tokenizer_repo: config.llm_tokenizer_repo.clone(),
            tokenizer_file: config.llm_tokenizer_file.clone(),
            max_tokens: config.llm_max_tokens as usize,
            temperature: f64::from(config.llm_temperature),
            seed: config.llm_seed,
            system_prompt: config.llm_system_prompt.clone(),
        }
    }

    #[instrument(skip(self))]
    pub async fn load(&self) -> Result<()> {
        info!(repo = %self.repo, revision = %self.revision, "Loading Chronicle LLM");
        let lease = self.runtime.begin_llm_load()?;
        let repo = self.repo.clone();
        let revision = self.revision.clone();
        let model_file = self.model_file.clone();
        let tokenizer_repo = self.tokenizer_repo.clone();
        let tokenizer_file = self.tokenizer_file.clone();

        let loaded = tokio::task::spawn_blocking(move || {
            LoadedLlm::load(
                &repo,
                &revision,
                &model_file,
                &tokenizer_repo,
                &tokenizer_file,
            )
        })
        .await
        .context("Native LLM loading task failed")??;

        let mut model = self
            .model
            .lock()
            .map_err(|_| anyhow!("LLM model state is poisoned"))?;

        if model.is_some() {
            bail!("The LLM model was loaded concurrently");
        }

        *model = Some(loaded);
        drop(model);
        lease.commit_to_loaded()?;

        info!("Chronicle LLM loaded");

        Ok(())
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

        tokio::task::spawn_blocking(move || drop(model))
            .await
            .context("Native LLM unload task failed")?;

        lease.commit_to_idle()?;
        info!("Chronicle LLM unloaded");
        Ok(())
    }

    #[instrument(skip(self, prompt), fields(prompt_len = prompt.len()))]
    pub async fn generate(&self, prompt: &str) -> Result<String> {
        debug!(
            max_tokens = self.max_tokens,
            temperature = self.temperature,
            "Starting LLM inference"
        );
        let model = Arc::clone(&self.model);
        let system_prompt = self.system_prompt.clone();
        let user_prompt = format!(
            "<|im_start|>system\n{system_prompt}<|im_end|>\n<|im_start|>user\n{prompt}<|im_end|>\n<|im_start|>assistant\n"
        );
        let max_tokens = self.max_tokens;
        let temperature = self.temperature;
        let seed = self.seed;

        tokio::task::spawn_blocking(move || {
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
            loaded.model.clear_kv_cache();

            let response = response.trim().to_owned();
            tracing::debug!(
                response_len = response.len(),
                generated_tokens = generated.len(),
                "LLM inference complete"
            );
            Ok(response)
        })
        .await
        .context("Native LLM inference task failed")?
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
