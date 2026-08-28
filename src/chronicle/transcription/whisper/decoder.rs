use crate::chronicle::transcription::whisper::transcriber::WhisperTranscriber;

use anyhow::{Result, anyhow};
use candle_core::{IndexOp, Tensor};
use candle_nn::ops::softmax;

pub struct Decoded {
    pub tokens: Vec<u32>,
    pub text: String,
    pub avg_logprob: f64,
    pub no_speech_prob: f64,
}

impl WhisperTranscriber {
    pub fn decode_segment(&mut self, mel: &Tensor) -> Result<Decoded> {
        let audio_features = self.model.encoder_forward(mel, true)?;

        let sample_len = self.model.config().max_target_positions / 2;

        let mut tokens = vec![self.sot_token, self.language_token, self.transcribe_token];

        let mut sum_logprob = 0.0f64;
        let mut no_speech_prob = f64::NAN;

        for i in 0..sample_len {
            let tokens_tensor = Tensor::new(tokens.as_slice(), mel.device())?.unsqueeze(0)?;

            let ys = self
                .model
                .decoder_forward(&tokens_tensor, &audio_features, i == 0)?;

            if i == 0 {
                let logits = self.model.decoder_final_linear(&ys.i(..1)?)?.i(0)?.i(0)?;

                no_speech_prob = f64::from(softmax(&logits, 0)?
                    .i(self.no_speech_token as usize)?
                    .to_scalar::<f32>()?);
            }

            let (_, seq_len, _) = ys.dims3()?;

            let logits = self
                .model
                .decoder_final_linear(&ys.i((..1, seq_len - 1..))?)?
                .i(0)?
                .i(0)?;

            let logits = logits.broadcast_add(&self.suppress_tokens)?;

            let logits_vec = logits.to_vec1::<f32>()?;

            let next_token = u32::try_from(
                logits_vec
                .iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| a.total_cmp(b))
                .map(|(index, _)| index)
                .ok_or_else(|| anyhow!("Whisper produced no logits"))?,
            )?;

            let probability = softmax(&logits, 0)?
                .i(next_token as usize)?
                .to_scalar::<f32>()?;

            if probability > 0.0 {
                sum_logprob += f64::from(probability).ln();
            }

            tokens.push(next_token);

            if next_token == self.eot_token
                || tokens.len() > self.model.config().max_target_positions
            {
                break;
            }
        }

        let text = self
            .tokenizer
            .decode(&tokens, true)
            .map_err(|error| anyhow!("Tokenizer decode failed: {error}"))?;

        let token_count = u32::try_from(tokens.len().max(1))?;
        let avg_logprob = sum_logprob / f64::from(token_count);

        Ok(Decoded {
            tokens,
            text,
            avg_logprob,
            no_speech_prob,
        })
    }
}
