use std::{
    collections::{HashMap, hash_map::Entry}, path::{Path, PathBuf}, sync::Arc,
};

use chrono::{
    prelude::Local,
    DateTime,
};
use rtrb::{
    Producer,
    RingBuffer
};
use serde::{Deserialize, Serialize};
use serenity::model::id::GuildId;
use tokio::{
    sync::{
        oneshot,
        Mutex
    },
    task::JoinHandle,
};
use serenity::all::UserId;
use songbird::{Call, CoreEvent, events::{
    Event,
    EventContext,
    EventHandler
}};

use crate::{
    chronicle::encoder::run_encoder,
    constants::{
        RING_BUFFER_CAPACITY,
        SILENCE_FRAME},
    discord::context::Error,
};

#[derive(Debug, Serialize, Deserialize)]
pub struct RecordingManifest {
    pub guild_id: GuildId,
    pub started_at: DateTime<Local>,
    pub ended_at: DateTime<Local>,
    pub participants: Vec<UserId>,
}
impl RecordingManifest {
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&contents)?)
    }
}

pub struct UserRecording {
    pub producer: Producer<i16>,
    pub stop_tx: oneshot::Sender<()>,
    pub encoder: JoinHandle<Result<(), Error>>,
}

pub struct RecordingSession {
    pub guild_id: GuildId,
    pub started_at: DateTime<Local>,
    pub session_name: String,
    pub tick: u64,
    pub users: HashMap<UserId, UserRecording>,
}
pub struct RecorderManager {
    recorders: Arc<Mutex<HashMap<GuildId, Recorder>>>,
}

impl RecorderManager {
    pub fn new() -> Self {
        Self {
            recorders: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn get(&self, guild_id: GuildId) -> Option<Recorder> {
        self.recorders.lock().await.get(&guild_id).cloned()
    }

    pub async fn get_or_create(
        &self,
        guild_id: GuildId,
    ) -> (Recorder, bool) {
        let mut recorders = self.recorders.lock().await;

        match recorders.entry(guild_id) {
            Entry::Occupied(entry) => {
                (entry.get().clone(), false)
            }
            Entry::Vacant(entry) => {
                let recorder = Recorder::new();
                entry.insert(recorder.clone());
                (recorder, true)
            }
        }
    }

    pub async fn remove(&self, guild_id: GuildId) -> Option<Recorder> {
        self.recorders.lock().await.remove(&guild_id)
    }
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

    pub async fn start_recording(
        &self,
        guild_id: GuildId,
        session_name: String,
    ) -> Result<bool, Error> {
        let started_at = Local::now();
        ensure_recording_directory(guild_id, &session_name, started_at)?;

        let mut recording = self.recording_session.lock().await;

        if recording.is_some() {
            return Ok(false);
        }

        *recording = Some(RecordingSession {
            guild_id,
            started_at,
            session_name,
            tick: 0,
            users: HashMap::new(),
        });

        tracing::info!(
            guild_id = %guild_id,
            "Recording started (id: {})",
            self.id
        );

        Ok(true)
    }

    pub async fn stop_recording(&self) -> Result<bool, Error> {

        let session = {
            let mut recording = self.recording_session.lock().await;

            let Some(session) = recording.take() else {
                return Ok(false);
            };

            session
        };

        let participants: Vec<UserId> = session.users.keys().copied().collect();

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

        let manifest: RecordingManifest = RecordingManifest { 
            guild_id: session.guild_id,
            started_at: session.started_at,
            ended_at: Local::now(),
            participants 
        };

        let manifest_path = recording_directory(
            session.guild_id,
            &session.session_name,
            session.started_at,
        )
        .join("manifest.toml");

        let manifest_toml = toml::to_string_pretty(&manifest)?;

        std::fs::write(&manifest_path, manifest_toml)?;

        tracing::info!(
            path = %manifest_path.display(),
            "Recording manifest written"
        );

        Ok(true)
    }

    pub async fn is_recording(&self) -> bool {
        self.recording_session.lock().await.is_some()
    }

    fn initiate_user_recording(
        &self,
        guild_id: GuildId,
        user_id: UserId,
        started_at: DateTime<Local>,
        session_name: String,
        initial_silence_ticks: u64,
    ) -> Result<UserRecording, Error> {
        let (producer, consumer) =
            RingBuffer::<i16>::new(RING_BUFFER_CAPACITY);

        let (stop_tx, stop_rx) = oneshot::channel();

        let path = recording_path(guild_id, user_id, &session_name, started_at);

        let encoder = tokio::task::spawn_blocking(move || {
            run_encoder(
                user_id,
                path,
                consumer,
                stop_rx,
                initial_silence_ticks,
            )
        });

        Ok(UserRecording {
            producer,
            stop_tx,
            encoder,
        })
    }
    
    pub async fn attach_to_call(
        &self,
        call: &Arc<Mutex<Call>>,
    ) -> Result<(), Error> {

        let mut call_lock = call.lock().await;

        call_lock.add_global_event(
            CoreEvent::SpeakingStateUpdate.into(),
            self.clone(),
        );

        call_lock.add_global_event(
            CoreEvent::VoiceTick.into(),
            self.clone(),
        );

        Ok(())
    }

}

#[async_trait::async_trait]
impl EventHandler for Recorder {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {

        match ctx {
            EventContext::SpeakingStateUpdate(state) => {
                tracing::debug!(?state, "RAW SpeakingStateUpdate");
                // tracing::debug!(
                //     ssrc = state.ssrc,
                //     user_id = ?state.user_id,
                //     "Recorder received SpeakingStateUpdate"
                // );

                if let Some(user_id_svm) = state.user_id {
                    let mut mappings = self.ssrc_to_user.lock().await;

                    mappings.insert(state.ssrc, UserId::new(user_id_svm.0));
                }
            }

            EventContext::VoiceTick(tick) => {
                // tracing::debug!(
                //     speaking_users = tick.speaking.len(),
                //     "Recorder received VoiceTick"
                // );

                // for (&ssrc, voice_data) in &tick.speaking {
                //     tracing::debug!(
                //         ssrc,
                //         has_decoded_voice = voice_data.decoded_voice.is_some(),
                //         "VoiceTick audio"
                //     );
                // }

                // Build a map of users that actually have audio during this tick.
                //
                // We do this before locking recording_session so that we don't
                // need to hold both locks while resolving SSRCs.
                let mut tick_audio = HashMap::<UserId, &[i16]>::new();

                {
                    let mappings = self.ssrc_to_user.lock().await;

                    for (&ssrc, voice_data) in &tick.speaking {
                        let Some(audio) = &voice_data.decoded_voice else {
                            continue;
                        };

                        let Some(user_id) = mappings.get(&ssrc).copied() else {
                            continue;
                        };

                        tick_audio.insert(user_id, audio);
                    }
                }

                let mut recording = self.recording_session.lock().await;

                let Some(session) = recording.as_mut() else {
                    return None;
                };

                // Create recordings for users who have just started speaking.
                for &user_id in tick_audio.keys() {
                    if session.users.contains_key(&user_id) {
                        continue;
                    }

                    let user_recording = match self
                        .initiate_user_recording(
                            session.guild_id,
                            user_id,
                            session.started_at,
                            session.session_name.clone(),
                            session.tick,
                        )

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

                    session.users.insert(user_id, user_recording);
                }

                // Every user gets exactly one 20 ms PCM frame per VoiceTick.
                //
                // If Songbird supplied audio, write that audio.
                // Otherwise, write 20 ms of silence.
                for (&user_id, user_recording) in &mut session.users {
                    let audio = tick_audio
                        .get(&user_id)
                        .copied()
                        .unwrap_or(&SILENCE_FRAME);

                    write_pcm(
                        &mut user_recording.producer,
                        audio,
                        user_id,
                    );
                }

                // Advance our recording timeline by one 20 ms tick.
                session.tick += 1;
            }

            _ => {}
        }

        None
    }
}

fn write_pcm(
    producer: &mut Producer<i16>,
    samples: &[i16],
    user_id: UserId,
) {
    if producer.slots() < samples.len() {
        tracing::warn!(
            ?user_id,
            available = producer.slots(),
            required = samples.len(),
            "Recording ring buffer full; dropping PCM frame"
        );
        return;
    }

    match producer.write_chunk(samples.len()) {
        Ok(mut chunk) => {
            let (first, second) = chunk.as_mut_slices();
            let first_len = first.len();

            first.copy_from_slice(&samples[..first_len]);

            if !second.is_empty() {
                second.copy_from_slice(&samples[first_len..]);
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

fn recording_directory(
    guild_id: GuildId,
    session_name: &str,
    started_at: DateTime<Local>,
) -> PathBuf {
    PathBuf::from(format!(
        ".chronicle/recordings/{}/{}-{}",
        guild_id,
        started_at.format("%Y%m%d-%H%M%S"),
        session_name,
    ))
}

fn recording_path(
    guild_id: GuildId,
    user_id: UserId,
    session_name: &str,
    started_at: DateTime<Local>,
) -> PathBuf {
    recording_directory(guild_id, session_name, started_at)
        .join(format!("recording-{}.opus", user_id))
}

fn ensure_recording_directory(
    guild_id: GuildId,
    session_name: &str,
    started_at: DateTime<Local>,
) -> Result<(), std::io::Error> {
    std::fs::create_dir_all(
        recording_directory(guild_id, session_name, started_at),
    )
}