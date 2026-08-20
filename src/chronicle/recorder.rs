use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
};

use rtrb::{
    Producer,
    RingBuffer
};
use tokio::{
    sync::{
        oneshot,
        Mutex
    },
    task::JoinHandle,
};
use serenity_voice_model::id::UserId;
use songbird::events::{
    Event,
    EventContext,
    EventHandler
};

use crate::{
    chronicle::encoder::run_encoder,
    constants::RING_BUFFER_CAPACITY,
    definitions::Error,
};

pub struct RecordingSession {
    pub users: HashMap<UserId, UserRecording>,
}

pub struct UserRecording {
    pub producer: Producer<i16>,
    pub stop_tx: oneshot::Sender<()>,
    pub encoder: JoinHandle<Result<(), Error>>,
}

#[derive(Clone)]
pub struct Recorder {
    pub id: u64,
    pub ssrc_to_user: Arc<Mutex<HashMap<u32, UserId>>>,
    pub recording: Arc<Mutex<Option<RecordingSession>>>,
}

impl Recorder {
    pub fn new() -> Self {
        Self {
            id: rand::random(),
            ssrc_to_user: Arc::new(Mutex::new(HashMap::new())),
            recording: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn start_recording(&self) -> Result<bool, Error> {
        ensure_recording_directory()?;

        let mut recording = self.recording.lock().await;

        if recording.is_some() {
            return Ok(false);
        }

        *recording = Some(RecordingSession {
            users: HashMap::new(),
        });

        tracing::info!("Recording started (id: {})", self.id);

        Ok(true)
    }

    pub async fn stop_recording(&self) -> Result<bool, Error> {
        let session = {
            let mut recording = self.recording.lock().await;

            let Some(session) = recording.take() else {
                return Ok(false);
            };

            session
        };

        tracing::info!(
            users = session.users.len(),
            "Stopping recording (id: {})", self.id
        );

        for (_, user_recording) in session.users {
            let UserRecording {
                producer,
                stop_tx,
                encoder,
            } = user_recording;

            // Tell the encoder that no more data should be expected.
            let _ = stop_tx.send(());

            // The producer must remain alive while the encoder drains the
            // samples already committed to the ring buffer. Once the
            // encoder has been told to stop, dropping the producer is safe.
            drop(producer);

            match encoder.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    tracing::error!(%error, "User recording encoder failed");
                }
                Err(error) => {
                    tracing::error!(%error, "User recording encoder task failed");
                }
            }
        }

        tracing::info!("Recording stopped");

        Ok(true)
    }

    pub async fn is_recording(&self) -> bool {
        self.recording.lock().await.is_some()
    }

    async fn initiate_user_recording(
        &self,
        user_id: UserId,
    ) -> Result<UserRecording, Error> {
        let (producer, consumer) =
            RingBuffer::<i16>::new(RING_BUFFER_CAPACITY);

        let (stop_tx, stop_rx) = oneshot::channel();

        let path = recording_path(user_id);

        let encoder = tokio::task::spawn_blocking(move || {
            run_encoder(
                user_id,
                path,
                consumer,
                stop_rx,
            )
        });

        Ok(UserRecording {
            producer,
            stop_tx,
            encoder,
        })
    }

}

#[async_trait::async_trait]
impl EventHandler for Recorder {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {

        match ctx {
            EventContext::SpeakingStateUpdate(state) => {

                if let Some(user_id) = state.user_id {
                    let mut mappings = self.ssrc_to_user.lock().await;

                    mappings.insert(state.ssrc, user_id);
                }
            }

            EventContext::VoiceTick(tick) => {
                for (&ssrc, voice_data) in &tick.speaking {
                    let Some(audio) = &voice_data.decoded_voice else {
                        continue;
                    };

                    let user_id = {
                        let mappings = self.ssrc_to_user.lock().await;
                        mappings.get(&ssrc).copied()
                    };

                    let Some(user_id) = user_id else {
                        continue;
                    };

                    let mut recording = self.recording.lock().await;

                    let Some(session) = recording.as_mut() else {
                        // Bot is receiving voice, but recording hasn't been
                        // requested.
                        continue;
                    };

                    if !session.users.contains_key(&user_id) {
                        let user_recording = match self.initiate_user_recording(user_id).await {
                            Ok(recording) => recording,

                            Err(error) => {
                                tracing::error!(
                                    %error,
                                    ?user_id,
                                    "Failed to create user recording"
                                );

                                continue;
                            }
                        };

                        session.users.insert(user_id, user_recording);

                        tracing::info!(
                            ?user_id,
                            "Started recording user"
                        );
                    }

                    let user_recording = session
                        .users
                        .get_mut(&user_id)
                        .expect("user recording was just inserted");

                    if user_recording.producer.slots() < audio.len() {
                        tracing::warn!(
                            ?user_id,
                            available = user_recording.producer.slots(),
                            required = audio.len(),
                            "Recording ring buffer full; dropping PCM frame"
                        );

                        continue;
                    }

                    match user_recording.producer.write_chunk(audio.len()) {
                        Ok(mut chunk) => {
                            let (first, second) = chunk.as_mut_slices();

                            let first_len = first.len();

                            first.copy_from_slice(&audio[..first_len]);

                            if !second.is_empty() {
                                second.copy_from_slice(&audio[first_len..]);
                            }

                            chunk.commit_all();
                        }

                        Err(error) => {
                            tracing::warn!(
                                ?user_id,
                                ?error,
                                "Failed to write PCM to recording ring buffer"
                            );
                        }
                    }
                }
            }

            _ => {}
        }

        None
    }
}

fn recording_path(user_id: UserId) -> PathBuf {
    PathBuf::from(format!(
        ".chronicle/audio/recording-{}.opus",
        user_id
    ))
}

fn ensure_recording_directory() -> Result<(), std::io::Error> {
    std::fs::create_dir_all(".chronicle/audio")
}