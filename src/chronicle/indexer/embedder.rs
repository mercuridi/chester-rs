// src/chronicle/indexer/embedder.rs

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use candle_core::{Device, IndexOp, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{self, BertModel, Config};
use hf_hub::{Repo, RepoType, api::sync::Api};
use tokenizers::{PaddingParams, PaddingStrategy, Tokenizer, TruncationParams};

const MODEL_ID: &str = "BAAI/bge-small-en-v1.5";

pub const EMBEDDING_DIMENSIONS: usize = 384;
const MAX_SEQUENCE_LENGTH: usize = 512;

pub struct Embedder {
    model: BertModel,
    tokenizer: Tokenizer,
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

        tokenizer.with_padding(Some(PaddingParams {
            strategy: PaddingStrategy::BatchLongest,
            ..Default::default()
        }));

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
            device,
        })
    }

    pub fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(|error| anyhow!("Failed to tokenize text: {error}"))?;

        let input_ids = Tensor::new(encoding.get_ids(), &self.device)
            .context("Failed to create input ID tensor")?
            .unsqueeze(0)
            .context("Failed to add batch dimension to input IDs")?;

        let token_type_ids = Tensor::new(encoding.get_type_ids(), &self.device)
            .context("Failed to create token type ID tensor")?
            .unsqueeze(0)
            .context("Failed to add batch dimension to token type IDs")?;

        let attention_mask = Tensor::new(encoding.get_attention_mask(), &self.device)
            .context("Failed to create attention mask tensor")?
            .unsqueeze(0)
            .context("Failed to add batch dimension to attention mask")?;

        let hidden_states = self
            .model
            .forward(&input_ids, &token_type_ids, Some(&attention_mask))
            .context("BGE forward pass failed")?;

        // BGE-small-en-v1.5 uses CLS pooling.
        let embedding = hidden_states
            .i((0, 0))
            .context("Failed to extract CLS embedding")?;

        self.normalize(embedding)
    }

    pub fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        texts.iter().map(|text| self.embed(text)).collect()
    }

    fn normalize(&self, embedding: Tensor) -> Result<Vec<f32>> {
        let norm = embedding
            .sqr()
            .context("Failed to square embedding")?
            .sum_all()
            .context("Failed to calculate embedding norm")?
            .sqrt()
            .context("Failed to calculate embedding norm")?;

        let embedding = embedding
            .broadcast_div(&norm)
            .context("Failed to normalize embedding")?;

        let embedding = embedding
            .to_vec1::<f32>()
            .context("Failed to copy embedding from device")?;

        if embedding.len() != EMBEDDING_DIMENSIONS {
            return Err(anyhow!(
                "Expected embedding dimension {}, got {}",
                EMBEDDING_DIMENSIONS,
                embedding.len()
            ));
        }

        Ok(embedding)
    }
}
