use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
};

use chrono::prelude::Local;
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

pub struct UserRecording {
    pub producer: Producer<i16>,
    pub stop_tx: oneshot::Sender<()>,
    pub encoder: JoinHandle<Result<(), Error>>,
}

pub struct RecordingSession {
    pub users: HashMap<UserId, UserRecording>,
    pub start: Arc<String>,
}


#[derive(Clone)]
pub struct Recorder {
    pub id: u64,
    pub ssrc_to_user: Arc<Mutex<HashMap<u32, UserId>>>,
    pub recording_session: Arc<Mutex<Option<RecordingSession>>>,
}

impl Recorder {
    pub fn new() -> Self {
        Self {
            id: rand::random(),
            ssrc_to_user: Arc::new(Mutex::new(HashMap::new())),
            recording_session: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn start_recording(&self) -> Result<bool, Error> {

        let start = Local::now().format("%Y%m%d-%H%M%S").to_string();

        ensure_recording_directory(&start).unwrap();

        let mut recording = self.recording_session.lock().await;

        if recording.is_some() {
            return Ok(false);
        }

        *recording = Some(RecordingSession {
            users: HashMap::new(),
            start: Arc::new(start),
        });

        tracing::info!("Recording started (id: {})", self.id);

        Ok(true)
    }

    pub async fn stop_recording(&self) -> Result<bool, Error> {

        tracing::debug!("stop_recording: waiting for recording lock");

        let session = {
            let mut recording = self.recording_session.lock().await;

            tracing::debug!("stop_recording: acquired recording lock");

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
        self.recording_session.lock().await.is_some()
    }

    async fn initiate_user_recording(
        &self,
        user_id: UserId,
        start: Arc<String>,
    ) -> Result<UserRecording, Error> {
        let (producer, consumer) =
            RingBuffer::<i16>::new(RING_BUFFER_CAPACITY);

        let (stop_tx, stop_rx) = oneshot::channel();

        let path = recording_path(user_id, &start);

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

                    tracing::debug!("VoiceTick: waiting for recording lock");

                    let mut recording = self.recording_session.lock().await;

                    tracing::debug!("VoiceTick: acquired recording lock");

                    let Some(session) = recording.as_mut() else {
                        tracing::debug!("VoiceTick: recording is inactive");
                        continue;
                    };

                    tracing::debug!(
                        ?user_id,
                        existing_users = session.users.len(),
                        "VoiceTick: checking user recording"
                    );

                    if !session.users.contains_key(&user_id) {
                        tracing::debug!(
                            ?user_id,
                            "VoiceTick: initiating user recording"
                        );

                        let user_recording = match self
                            .initiate_user_recording(user_id, session.start.clone())
                            .await
                        {
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

                        tracing::debug!(
                            ?user_id,
                            "VoiceTick: user recording initiated"
                        );

                        session.users.insert(user_id, user_recording);

                        tracing::info!(
                            ?user_id,
                            "Started recording user"
                        );
                    }

                    tracing::debug!(
                        ?user_id,
                        "VoiceTick: getting user recording"
                    );

                    let user_recording = session
                        .users
                        .get_mut(&user_id)
                        .expect("user recording was just inserted");

                    tracing::debug!(
                        ?user_id,
                        samples = audio.len(),
                        available = user_recording.producer.slots(),
                        "VoiceTick: about to write PCM"
                    );

                    if user_recording.producer.slots() < audio.len() {
                        tracing::warn!(
                            ?user_id,
                            available = user_recording.producer.slots(),
                            required = audio.len(),
                            "Recording ring buffer full; dropping PCM frame"
                        );

                        continue;
                    }
                    
                    tracing::debug!(
                        ?user_id,
                        samples = audio.len(),
                        "VoiceTick: writing PCM"
                    );

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

                    tracing::debug!(
                        ?user_id,
                        "VoiceTick: PCM write complete"
                    );
                }
            }

            _ => {}
        }

        None
    }
}

fn recording_path(user_id: UserId, stamp: &str) -> PathBuf {
    PathBuf::from(format!(
        ".chronicle/recordings/{}/recording-{}.opus",
        stamp,
        user_id,
    ))
}

fn ensure_recording_directory(stamp: &str) -> Result<(), std::io::Error> {
    std::fs::create_dir_all(format!(".chronicle/recordings/{}", stamp))
}