use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use candle_core::{Device, IndexOp, Tensor};
use candle_nn::{
    ops::{log_softmax, softmax},
    VarBuilder,
};
use candle_transformers::models::whisper::{self as m, audio, Config};
use hf_hub::{api::sync::Api, Repo, RepoType};
use tokenizers::Tokenizer;

use super::audio::Audio;

const MODEL_ID: &str = "openai/whisper-small.en";
const MODEL_REVISION: &str = "refs/pr/10";
const MODEL_SAMPLE_RATE: u32 = m::SAMPLE_RATE as u32;

pub struct TranscriptSegment {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

pub struct WhisperTranscriber {
    model: Model,
    tokenizer: Tokenizer,
    device: Device,

    suppress_tokens: Tensor,

    sot_token: u32,
    transcribe_token: u32,
    eot_token: u32,
    no_speech_token: u32,
    no_timestamps_token: u32,
}

enum Model {
    Normal(m::model::Whisper),
}

impl Model {
    fn config(&self) -> &Config {
        match self {
            Self::Normal(model) => &model.config,
        }
    }

    fn encoder_forward(
        &mut self,
        input: &Tensor,
        flush: bool,
    ) -> candle_core::Result<Tensor> {
        match self {
            Self::Normal(model) => model.encoder.forward(input, flush),
        }
    }

    fn decoder_forward(
        &mut self,
        tokens: &Tensor,
        audio_features: &Tensor,
        flush: bool,
    ) -> candle_core::Result<Tensor> {
        match self {
            Self::Normal(model) => {
                model.decoder.forward(tokens, audio_features, flush)
            }
        }
    }

    fn decoder_final_linear(
        &self,
        input: &Tensor,
    ) -> candle_core::Result<Tensor> {
        match self {
            Self::Normal(model) => model.decoder.final_linear(input),
        }
    }
}

impl WhisperTranscriber {
    /// Load Whisper Small English onto CUDA device 0.
    ///
    /// The first invocation downloads the model through hf-hub.
    /// Subsequent invocations use the local Hugging Face cache.
    pub fn new_cuda() -> Result<Self> {
        let device = Device::new_cuda(0)
            .context("Failed to initialize CUDA device 0")?;

        Self::load(device)
    }

    fn load(device: Device) -> Result<Self> {
        tracing::info!(
            device = ?device,
            model = MODEL_ID,
            revision = MODEL_REVISION,
            "Loading Whisper model"
        );

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

        tracing::info!(
            config = ?config_path,
            tokenizer = ?tokenizer_path,
            weights = ?weights_path,
            "Whisper model files ready"
        );

        let config: Config = serde_json::from_str(
            &std::fs::read_to_string(&config_path)
                .context("Failed to read Whisper config")?,
        )
        .context("Failed to parse Whisper config")?;

        if config.num_mel_bins != 80 {
            return Err(anyhow!(
                "Expected 80 mel bins for whisper-small.en, got {}",
                config.num_mel_bins
            ));
        }

        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|error| anyhow!("Failed to load tokenizer: {error}"))?;

        let model = unsafe {
            VarBuilder::from_mmaped_safetensors(
                &[PathBuf::from(&weights_path)],
                m::DTYPE,
                &device,
            )
        }
        .context("Failed to memory-map Whisper weights")?;

        let model = m::model::Whisper::load(&model, config)?;

        let model = Model::Normal(model);

        let no_timestamps_token =
            token_id(&tokenizer, m::NO_TIMESTAMPS_TOKEN)?;

        let suppress_tokens: Vec<f32> = (0..model.config().vocab_size as u32)
            .map(|token| {
                if model.config().suppress_tokens.contains(&token)
                    || token == no_timestamps_token
                {
                    f32::NEG_INFINITY
                } else {
                    0.0
                }
            })
            .collect();

        let suppress_tokens =
            Tensor::new(suppress_tokens.as_slice(), &device)?;

        let sot_token = token_id(&tokenizer, m::SOT_TOKEN)?;
        let transcribe_token =
            token_id(&tokenizer, m::TRANSCRIBE_TOKEN)?;
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
            transcribe_token,
            eot_token,
            no_speech_token,
            no_timestamps_token,
        })
    }

    /// Transcribe already-decoded 16 kHz mono audio.
    pub fn transcribe(&mut self, audio: &Audio) -> Result<Vec<TranscriptSegment>> {
        if audio.sample_rate != MODEL_SAMPLE_RATE {
            return Err(anyhow!(
                "Whisper expects {} Hz audio, got {} Hz",
                MODEL_SAMPLE_RATE,
                audio.sample_rate
            ));
        }

        if audio.samples.is_empty() {
            return Ok(Vec::new());
        }

        let mel_filters = load_mel_filters()?;

        let mel = audio::pcm_to_mel(
            self.model.config(),
            &audio.samples,
            &mel_filters,
        );

        let mel_len = mel.len();

        let mel = Tensor::from_vec(
            mel,
            (
                1,
                self.model.config().num_mel_bins,
                mel_len / self.model.config().num_mel_bins,
            ),
            &self.device,
        )?;

        self.decode_mel(&mel)
    }

    fn decode_mel(&mut self, mel: &Tensor) -> Result<Vec<TranscriptSegment>> {
        let (_, _, content_frames) = mel.dims3()?;

        let mut seek = 0;
        let mut segments = Vec::new();

        while seek < content_frames {
            let segment_size =
                usize::min(content_frames - seek, m::N_FRAMES);

            let mel_segment =
                mel.narrow(2, seek, segment_size)?;

            let segment_start =
                (seek * m::HOP_LENGTH) as f64 / m::SAMPLE_RATE as f64;

            let segment_duration =
                (segment_size * m::HOP_LENGTH) as f64
                    / m::SAMPLE_RATE as f64;

            let decoded = self.decode_segment(&mel_segment)?;

            seek += segment_size;

            if decoded.no_speech_prob > m::NO_SPEECH_THRESHOLD
                && decoded.avg_logprob < m::LOGPROB_THRESHOLD
            {
                continue;
            }

            let timestamp_segments =
                self.extract_timestamp_segments(
                    &decoded.tokens,
                    segment_start,
                )?;

            if timestamp_segments.is_empty() {
                let text = decoded.text.trim();

                if !text.is_empty() {
                    segments.push(TranscriptSegment {
                        start: segment_start,
                        end: segment_start + segment_duration,
                        text: text.to_owned(),
                    });
                }
            } else {
                segments.extend(timestamp_segments);
            }
        }

        Ok(segments)
    }

    fn decode_segment(
        &mut self,
        mel: &Tensor,
    ) -> Result<Decoded> {
        let audio_features =
            self.model.encoder_forward(mel, true)?;

        let sample_len =
            self.model.config().max_target_positions / 2;

        let mut tokens = vec![self.sot_token, self.transcribe_token];

        let mut sum_logprob = 0.0f64;
        let mut no_speech_prob = f64::NAN;

        for i in 0..sample_len {
            let tokens_tensor =
                Tensor::new(tokens.as_slice(), mel.device())?
                    .unsqueeze(0)?;

            let ys = self.model.decoder_forward(
                &tokens_tensor,
                &audio_features,
                i == 0,
            )?;

            if i == 0 {
                let logits = self
                    .model
                    .decoder_final_linear(&ys.i(..1)?)?
                    .i(0)?
                    .i(0)?;

                no_speech_prob = softmax(&logits, 0)?
                    .i(self.no_speech_token as usize)?
                    .to_scalar::<f32>()?
                    as f64;
            }

            let (_, seq_len, _) = ys.dims3()?;

            let logits = self
                .model
                .decoder_final_linear(
                    &ys.i((..1, seq_len - 1..))?,
                )?
                .i(0)?
                .i(0)?;

            let logits =
                self.apply_timestamp_rules(&logits, &tokens)?;

            let logits =
                logits.broadcast_add(&self.suppress_tokens)?;

            let logits_vec = logits.to_vec1::<f32>()?;

            let next_token = logits_vec
                .iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| a.total_cmp(b))
                .map(|(index, _)| index as u32)
                .ok_or_else(|| anyhow!("Whisper produced no logits"))?;

            let probability =
                softmax(&logits, 0)?
                    .i(next_token as usize)?
                    .to_scalar::<f32>()?;

            if probability > 0.0 {
                sum_logprob += (probability as f64).ln();
            }

            tokens.push(next_token);

            if next_token == self.eot_token
                || tokens.len()
                    > self.model.config().max_target_positions
            {
                break;
            }
        }

        let text = self
            .tokenizer
            .decode(&tokens, true)
            .map_err(|error| anyhow!("Tokenizer decode failed: {error}"))?;

        let avg_logprob =
            sum_logprob / tokens.len().max(1) as f64;

        Ok(Decoded {
            tokens,
            text,
            avg_logprob,
            no_speech_prob,
        })
    }

    fn apply_timestamp_rules(
        &self,
        input_logits: &Tensor,
        tokens: &[u32],
    ) -> Result<Tensor> {
        let device = input_logits.device().clone();

        let timestamp_begin =
            self.no_timestamps_token + 1;

        let vocab_size =
            self.model.config().vocab_size as u32;

        let sample_begin = 2;

        let sampled_tokens = if tokens.len() > sample_begin {
            &tokens[sample_begin..]
        } else {
            &[]
        };

        let mut mask =
            vec![0.0f32; vocab_size as usize];

        // Timestamp tokens must alternate with text tokens.
        if let Some(&last) = sampled_tokens.last() {
            if last >= timestamp_begin {
                let previous_was_timestamp =
                    sampled_tokens.len() >= 2
                        && sampled_tokens[sampled_tokens.len() - 2]
                            >= timestamp_begin;

                if previous_was_timestamp {
                    for value in
                        mask[timestamp_begin as usize..].iter_mut()
                    {
                        *value = f32::NEG_INFINITY;
                    }
                } else {
                    for value in mask[..self.eot_token as usize]
                        .iter_mut()
                    {
                        *value = f32::NEG_INFINITY;
                    }
                }
            }
        }

        // Timestamp values must not decrease.
        let timestamp_tokens: Vec<u32> = sampled_tokens
            .iter()
            .copied()
            .filter(|&token| token >= timestamp_begin)
            .collect();

        if let Some(&last_timestamp) = timestamp_tokens.last() {
            let minimum_timestamp =
                if sampled_tokens.last()
                    .is_some_and(|&token| token >= timestamp_begin)
                {
                    last_timestamp
                } else {
                    last_timestamp + 1
                };

            for token in timestamp_begin..minimum_timestamp {
                mask[token as usize] = f32::NEG_INFINITY;
            }
        }

        // Whisper requires the first generated token to be a timestamp.
        if tokens.len() == sample_begin {
            for value in mask[..timestamp_begin as usize].iter_mut() {
                *value = f32::NEG_INFINITY;
            }
        }

        let mask_tensor =
            Tensor::new(mask.as_slice(), &device)?;

        let mut logits =
            input_logits.broadcast_add(&mask_tensor)?;

        // If the combined probability of timestamp tokens exceeds
        // the probability of the most likely text token, force a timestamp.
        let log_probs = log_softmax(&logits, 0)?;

        let timestamp_log_probs = log_probs.narrow(
            0,
            timestamp_begin as usize,
            vocab_size as usize - timestamp_begin as usize,
        )?;

        let text_log_probs =
            log_probs.narrow(0, 0, timestamp_begin as usize)?;

        let timestamp_max =
            timestamp_log_probs.max(0)?;

        let timestamp_logprob =
            timestamp_max
                .broadcast_add(
                    &timestamp_log_probs
                        .broadcast_sub(&timestamp_max)?
                        .exp()?
                        .sum(0)?
                        .log()?,
                )?
                .to_scalar::<f32>()?;

        let max_text_logprob =
            text_log_probs.max(0)?.to_scalar::<f32>()?;

        if timestamp_logprob > max_text_logprob {
            let mut force_timestamp =
                vec![0.0f32; vocab_size as usize];

            for value
                in force_timestamp[..timestamp_begin as usize].iter_mut()
            {
                *value = f32::NEG_INFINITY;
            }

            let force_timestamp =
                Tensor::new(force_timestamp.as_slice(), &device)?;

            logits =
                logits.broadcast_add(&force_timestamp)?;
        }

        Ok(logits)
    }

    fn extract_timestamp_segments(
        &self,
        tokens: &[u32],
        segment_start: f64,
    ) -> Result<Vec<TranscriptSegment>> {
        let mut output = Vec::new();
        let mut text_tokens = Vec::new();
        let mut previous_timestamp = 0.0f64;

        for &token in tokens {
            if token == self.sot_token
                || token == self.eot_token
            {
                continue;
            }

            if token > self.no_timestamps_token {
                let timestamp =
                    (token - self.no_timestamps_token + 1)
                        as f64
                        / 50.0;

                if !text_tokens.is_empty() {
                    let text = self
                        .tokenizer
                        .decode(&text_tokens, true)
                        .map_err(|error| {
                            anyhow!(
                                "Tokenizer decode failed: {error}"
                            )
                        })?
                        .trim()
                        .to_owned();

                    if !text.is_empty() {
                        output.push(TranscriptSegment {
                            start: segment_start
                                + previous_timestamp,
                            end: segment_start + timestamp,
                            text,
                        });
                    }

                    text_tokens.clear();
                }

                previous_timestamp = timestamp;
            } else {
                text_tokens.push(token);
            }
        }

        if !text_tokens.is_empty() {
            let text = self
                .tokenizer
                .decode(&text_tokens, true)
                .map_err(|error| {
                    anyhow!("Tokenizer decode failed: {error}")
                })?
                .trim()
                .to_owned();

            if !text.is_empty() {
                output.push(TranscriptSegment {
                    start: segment_start + previous_timestamp,
                    end: segment_start + m::CHUNK_LENGTH as f64,
                    text,
                });
            }
        }

        Ok(output)
    }
}

struct Decoded {
    tokens: Vec<u32>,
    text: String,
    avg_logprob: f64,
    no_speech_prob: f64,
}

fn token_id(
    tokenizer: &Tokenizer,
    token: &str,
) -> Result<u32> {
    tokenizer
        .token_to_id(token)
        .ok_or_else(|| anyhow!("No tokenizer ID for {token}"))
}

fn load_mel_filters() -> Result<Vec<f32>> {
    let bytes =
        // this filepath is fragile but tracks back 
        include_bytes!("melfilters/melfilters.bytes");

    if bytes.len() % 4 != 0 {
        return Err(anyhow!(
            "melfilters.bytes length is not divisible by 4"
        ));
    }

    let mut filters =
        Vec::with_capacity(bytes.len() / 4);

    for chunk in bytes.chunks_exact(4) {
        filters.push(f32::from_le_bytes([
            chunk[0],
            chunk[1],
            chunk[2],
            chunk[3],
        ]));
    }

    Ok(filters)
}