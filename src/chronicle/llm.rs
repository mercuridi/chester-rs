use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct Llm {
    client: reqwest::Client,
    base_url: String,
    model: String,
    max_tokens: u32,
    temperature: f32,
}

impl Llm {
    pub fn new(
        base_url: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.into(),
            model: model.into(),
            max_tokens: 512,
            temperature: 0.2,
        }
    }

    pub async fn generate(&self, prompt: &str) -> Result<String> {
        let request = ChatCompletionRequest {
            model: self.model.clone(),
            messages: vec![ChatMessage {
                role: "user".to_owned(),
                content: prompt.to_owned(),
            }],
            max_tokens: self.max_tokens,
            temperature: self.temperature,
            stream: false,
        };

        let response = self
            .client
            .post(format!(
                "{}/v1/chat/completions",
                self.base_url.trim_end_matches('/')
            ))
            .json(&request)
            .send()
            .await
            .context("Failed to connect to llama-server")?;

        let response = response
            .error_for_status()
            .context("llama-server returned an error")?;

        let response: ChatCompletionResponse = response
            .json()
            .await
            .context("Failed to parse llama-server response")?;

        response
            .choices
            .into_iter()
            .next()
            .map(|choice| choice.message.content)
            .context("llama-server returned no completion")
    }
}

#[derive(Debug, Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<ChatMessage>,
    max_tokens: u32,
    temperature: f32,
    stream: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatCompletionChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionChoice {
    message: ChatMessage,
}