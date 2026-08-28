use crate::{
    chronicle::transcription::whisper::{tokens::token_id, transcriber::WhisperTranscriber},
    constants::{MODEL_ID, MODEL_REVISION},
};
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use candle_core::{Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::whisper::{self as m, Config};
use hf_hub::{Repo, RepoType, api::sync::Api};
use tokenizers::Tokenizer;

pub enum Model {
    Normal(m::model::Whisper),
}

impl Model {
    pub fn config(&self) -> &Config {
        match self {
            Self::Normal(model) => &model.config,
        }
    }

    pub fn encoder_forward(&mut self, input: &Tensor, flush: bool) -> candle_core::Result<Tensor> {
        match self {
            Self::Normal(model) => model.encoder.forward(input, flush),
        }
    }

    pub fn decoder_forward(
        &mut self,
        tokens: &Tensor,
        audio_features: &Tensor,
        flush: bool,
    ) -> candle_core::Result<Tensor> {
        match self {
            Self::Normal(model) => model.decoder.forward(tokens, audio_features, flush),
        }
    }

    pub fn decoder_final_linear(&self, input: &Tensor) -> candle_core::Result<Tensor> {
        match self {
            Self::Normal(model) => model.decoder.final_linear(input),
        }
    }
}

impl WhisperTranscriber {
    pub fn load(device: Device) -> Result<Self> {
        // tracing::info!(
        //     device = ?device,
        //     model = MODEL_ID,
        //     revision = MODEL_REVISION,
        //     "Loading Whisper model"
        // );

        let api = Api::new().context("Failed to initialize Hugging Face Hub")?;

        let repo = api.repo(Repo::with_revision(
            MODEL_ID.to_owned(),
            RepoType::Model,
            MODEL_REVISION.to_owned(),
        ));

        let config_path = repo
            .get("config.json")
            .context("Failed to download/load Whisper config.json")?;

        let tokenizer_path = repo
            .get("tokenizer.json")
            .context("Failed to download/load Whisper tokenizer.json")?;

        let weights_path = repo
            .get("model.safetensors")
            .context("Failed to download/load Whisper model.safetensors")?;

        // tracing::info!(
        //     config = ?config_path,
        //     tokenizer = ?tokenizer_path,
        //     weights = ?weights_path,
        //     "Whisper model files ready"
        // );

        let config: Config = serde_json::from_str(
            &std::fs::read_to_string(&config_path).context("Failed to read Whisper config")?,
        )
        .context("Failed to parse Whisper config")?;

        // if config.num_mel_bins != 128 {
        //     return Err(anyhow!(
        //         "Expected 128 mel bins for whisper, got {}",
        //         config.num_mel_bins
        //     ));
        // }

        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|error| anyhow!("Failed to load tokenizer: {error}"))?;

        let model = unsafe {
            VarBuilder::from_mmaped_safetensors(&[PathBuf::from(&weights_path)], m::DTYPE, &device)
        }
        .context("Failed to memory-map Whisper weights")?;

        let model = m::model::Whisper::load(&model, config)?;

        let model = Model::Normal(model);

        let no_timestamps_token = token_id(&tokenizer, m::NO_TIMESTAMPS_TOKEN)?;

        let vocab_size = u32::try_from(model.config().vocab_size)?;
        let suppress_tokens: Vec<f32> = (0..vocab_size)
            .map(|token| {
                if model.config().suppress_tokens.contains(&token) || token == no_timestamps_token {
                    f32::NEG_INFINITY
                } else {
                    0.0
                }
            })
            .collect();

        let suppress_tokens = Tensor::new(suppress_tokens.as_slice(), &device)?;

        let sot_token = token_id(&tokenizer, m::SOT_TOKEN)?;
        let lang_token = token_id(&tokenizer, "<|en|>")?;
        let transcribe_token = token_id(&tokenizer, m::TRANSCRIBE_TOKEN)?;
        let eot_token = token_id(&tokenizer, m::EOT_TOKEN)?;

        let no_speech_token = m::NO_SPEECH_TOKENS
            .iter()
            .find_map(|token| token_id(&tokenizer, token).ok())
            .ok_or_else(|| anyhow!("Unable to find Whisper no-speech token"))?;

        Ok(Self {
            model,
            tokenizer,
            device,
            suppress_tokens,
            sot_token,
            language_token: lang_token,
            transcribe_token,
            eot_token,
            no_speech_token,
            no_timestamps_token,
        })
    }
}
