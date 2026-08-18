use std::collections::HashMap;
use std::sync::Arc;

use serenity_voice_model::id::UserId;
use songbird::events::{Event, EventContext, EventHandler};
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct Receiver {
    pub ssrc_to_user: Arc<Mutex<HashMap<u32, UserId>>>,
}

impl Receiver {
    pub fn new() -> Self {
        Self {
            ssrc_to_user: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[async_trait::async_trait]
impl EventHandler for Receiver {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        match ctx {
            EventContext::SpeakingStateUpdate(state) => {
                if let Some(user_id) = state.user_id {
                    let mut mappings = self.ssrc_to_user.lock().await;

                    mappings.insert(state.ssrc, user_id);

                    tracing::debug!(
                        ssrc = state.ssrc,
                        user_id = %user_id,
                        "Mapped SSRC to user"
                    );
                }
            }

            EventContext::VoiceTick(tick) => {
                tracing::debug!(
                    speaking = tick.speaking.len(),
                    silent = tick.silent.len(),
                    "Voice tick received"
                );

                let mappings = self.ssrc_to_user.lock().await;

                for (&ssrc, voice_data) in &tick.speaking {
                    let Some(audio) = &voice_data.decoded_voice else {
                        tracing::debug!(
                            ssrc,
                            "Voice data has no decoded audio"
                        );
                        continue;
                    };

                    let user_id = mappings.get(&ssrc);

                    tracing::debug!(
                        ssrc,
                        user_id = ?user_id,
                        samples = audio.len(),
                        "Received decoded audio"
                    );
                }
            }

            _ => {}
        }

        None
    }
}