// src/chronicle/indexer/embedder.rs

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use candle_core::{Device, IndexOp, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{self, BertModel, Config};
use hf_hub::{Repo, RepoType, api::sync::Api};
use tokenizers::{Encoding, PaddingParams, PaddingStrategy, Tokenizer, TruncationParams};

const MODEL_ID: &str = "BAAI/bge-small-en-v1.5";

pub const EMBEDDING_DIMENSIONS: usize = 384;
const MAX_SEQUENCE_LENGTH: usize = 512;

pub struct Embedder {
    model: BertModel,
    tokenizer: Tokenizer,
    padding: PaddingParams,
    device: Device,
}

impl Embedder {
    pub fn load(device: Device) -> Result<Self> {
        tracing::info!(
            model = MODEL_ID,
            device = ?device,
            "Loading embedding model"
        );

        let api = Api::new().context("Failed to initialize Hugging Face Hub")?;

        let repo = api.repo(Repo::new(MODEL_ID.to_owned(), RepoType::Model));

        let config_path = repo
            .get("config.json")
            .context("Failed to download/load BGE config.json")?;

        let tokenizer_path = repo
            .get("tokenizer.json")
            .context("Failed to download/load BGE tokenizer.json")?;

        let weights_path = repo
            .get("model.safetensors")
            .context("Failed to download/load BGE model.safetensors")?;

        let config: Config = serde_json::from_str(
            &std::fs::read_to_string(&config_path).context("Failed to read BGE config")?,
        )
        .context("Failed to parse BGE config")?;

        if config.hidden_size != EMBEDDING_DIMENSIONS {
            return Err(anyhow!(
                "Expected BGE embedding dimension {}, got {}",
                EMBEDDING_DIMENSIONS,
                config.hidden_size
            ));
        }

        let mut tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|error| anyhow!("Failed to load BGE tokenizer: {error}"))?;

        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: MAX_SEQUENCE_LENGTH,
                ..Default::default()
            }))
            .map_err(|error| anyhow!("Failed to configure tokenizer truncation: {error}"))?;

        let padding = PaddingParams {
            strategy: PaddingStrategy::BatchLongest,
            ..Default::default()
        };

        let weights = unsafe {
            VarBuilder::from_mmaped_safetensors(
                &[PathBuf::from(&weights_path)],
                bert::DTYPE,
                &device,
            )
        }
        .context("Failed to memory-map BGE weights")?;

        let model = BertModel::load(weights, &config).context("Failed to load BGE model")?;

        tracing::info!("Embedding model loaded");

        Ok(Self {
            model,
            tokenizer,
            padding,
            device,
        })
    }

    pub fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let encoding = self.encode(text)?;
        self.embed_encodings(std::slice::from_ref(&encoding))?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("Embedding batch returned no embeddings"))
    }

    pub fn encode(&self, text: &str) -> Result<Encoding> {
        self.tokenizer
            .encode(text, true)
            .map_err(|error| anyhow!("Failed to tokenize text: {error}"))
    }

    pub fn tokenizer(&self) -> &Tokenizer {
        &self.tokenizer
    }

    pub fn embed_encodings(&self, encodings: &[Encoding]) -> Result<Vec<Vec<f32>>> {
        if encodings.is_empty() {
            return Ok(Vec::new());
        }

        let target_length = encodings
            .iter()
            .map(Encoding::len)
            .max()
            .ok_or_else(|| anyhow!("Embedding batch returned no encodings"))?;
        let mut padded_encodings = encodings.to_vec();
        for encoding in &mut padded_encodings {
            encoding.pad(
                target_length,
                self.padding.pad_id,
                self.padding.pad_type_id,
                &self.padding.pad_token,
                self.padding.direction,
            );
        }

        let input_ids = padded_encodings
            .iter()
            .map(|encoding| Tensor::new(encoding.get_ids(), &self.device))
            .collect::<candle_core::Result<Vec<_>>>()
            .context("Failed to create input ID tensors")?;
        let input_ids = Tensor::stack(&input_ids, 0).context("Failed to stack input ID tensors")?;

        let token_type_ids = padded_encodings
            .iter()
            .map(|encoding| Tensor::new(encoding.get_type_ids(), &self.device))
            .collect::<candle_core::Result<Vec<_>>>()
            .context("Failed to create token type ID tensors")?;
        let token_type_ids =
            Tensor::stack(&token_type_ids, 0).context("Failed to stack token type ID tensors")?;

        let attention_masks = padded_encodings
            .iter()
            .map(|encoding| Tensor::new(encoding.get_attention_mask(), &self.device))
            .collect::<candle_core::Result<Vec<_>>>()
            .context("Failed to create attention mask tensors")?;
        let attention_mask =
            Tensor::stack(&attention_masks, 0).context("Failed to stack attention mask tensors")?;

        let hidden_states = self
            .model
            .forward(&input_ids, &token_type_ids, Some(&attention_mask))
            .context("BGE forward pass failed")?;

        // BGE-small-en-v1.5 uses CLS pooling.
        let embeddings = hidden_states
            .i((.., 0))
            .context("Failed to extract CLS embeddings")?;

        Self::normalize_batch(&embeddings)
    }

    fn normalize_batch(embeddings: &Tensor) -> Result<Vec<Vec<f32>>> {
        let norms = embeddings
            .sqr()
            .context("Failed to square embeddings")?
            .sum(1)
            .context("Failed to calculate embedding norms")?
            .sqrt()
            .context("Failed to calculate embedding norms")?
            .unsqueeze(1)
            .context("Failed to add embedding norm dimension")?;

        let embeddings = embeddings
            .broadcast_div(&norms)
            .context("Failed to normalize embeddings")?;

        let embeddings = embeddings
            .to_vec2::<f32>()
            .context("Failed to copy embeddings from device")?;

        for embedding in &embeddings {
            if embedding.len() != EMBEDDING_DIMENSIONS {
                return Err(anyhow!(
                    "Expected embedding dimension {}, got {}",
                    EMBEDDING_DIMENSIONS,
                    embedding.len()
                ));
            }
        }

        Ok(embeddings)
    }
}
