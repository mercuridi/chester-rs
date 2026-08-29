use std::{
    collections::{HashMap, hash_map::Entry},
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::{DateTime, prelude::Local};
use rtrb::{Producer, RingBuffer};
use serde::{Deserialize, Serialize};
use serenity::all::UserId;
use serenity::{
    http::Http,
    model::id::{ChannelId, GuildId},
};
use songbird::{
    Call, CoreEvent,
    events::{Event, EventContext, EventHandler},
};
use tokio::{
    sync::{Mutex, oneshot},
    task::JoinHandle,
};

use crate::{
    chronicle::recording::encoder::run_encoder,
    constants::{RECORDINGS_DIR, RING_BUFFER_CAPACITY, SILENCE_FRAME},
    discord::context::Error,
};
use tracing::{debug, info, instrument, warn};

#[derive(Debug, Serialize, Deserialize)]
pub struct RecordingManifest {
    #[serde(default = "default_manifest_status")]
    pub status: ManifestStatus,
    pub guild_id: GuildId,
    pub started_at: DateTime<Local>,
    #[serde(default)]
    pub ended_at: Option<DateTime<Local>>,
    pub participants: Vec<UserId>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ManifestStatus {
    Recording,
    Complete,
}

fn default_manifest_status() -> ManifestStatus {
    ManifestStatus::Complete
}

impl RecordingManifest {
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&contents)?)
    }

    fn save_atomically(&self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        let path = path.as_ref();
        let temp_path = path.with_extension("toml.tmp");
        let contents = toml::to_string_pretty(self)?;

        std::fs::write(&temp_path, contents)?;

        let file = std::fs::OpenOptions::new().read(true).open(&temp_path)?;
        file.sync_data()?;
        drop(file);

        std::fs::rename(&temp_path, path)?;
        Ok(())
    }
}

pub struct UserRecording {
    pub producer: Producer<i16>,
    pub stop_tx: oneshot::Sender<()>,
    pub encoder: JoinHandle<Result<(), Error>>,
}

pub struct RecordingSession {
    pub guild_id: GuildId,
    pub voice_channel_id: ChannelId,
    pub notification_channel_id: ChannelId,
    pub initiator: UserId,
    pub started_at: DateTime<Local>,
    pub session_name: String,
    pub manifest_path: PathBuf,
    pub manifest: RecordingManifest,
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

    pub async fn get_or_create(&self, guild_id: GuildId) -> (Recorder, bool) {
        let mut recorders = self.recorders.lock().await;

        match recorders.entry(guild_id) {
            Entry::Occupied(entry) => (entry.get().clone(), false),
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

    #[instrument(skip(self), fields(session = %session_name))]
    pub async fn start_recording(
        &self,
        guild_id: GuildId,
        voice_channel_id: ChannelId,
        notification_channel_id: ChannelId,
        initiator: UserId,
        session_name: String,
    ) -> Result<bool, Error> {
        let started_at = Local::now();

        let recording_directory = recording_directory(guild_id, &session_name, started_at);
        ensure_recording_directory(guild_id, &session_name, started_at)?;

        let mut recording = self.recording_session.lock().await;

        if recording.is_some() {
            return Ok(false);
        }

        let manifest_path = recording_directory.join("manifest.toml");
        let manifest = RecordingManifest {
            status: ManifestStatus::Recording,
            guild_id,
            started_at,
            ended_at: None,
            participants: Vec::new(),
        };

        manifest.save_atomically(&manifest_path)?;

        *recording = Some(RecordingSession {
            guild_id,
            voice_channel_id,
            notification_channel_id,
            initiator,
            started_at,
            session_name,
            manifest_path,
            manifest,
            tick: 0,
            users: HashMap::new(),
        });

        tracing::info!(
            guild_id = %guild_id,
            voice_channel_id = %voice_channel_id,
            notification_channel_id = %notification_channel_id,
            initiator = %initiator,
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
        let guild_id = session.guild_id;
        let session_name = session.session_name.clone();
        info!(%guild_id, session = %session_name, participant_count = participants.len(), "Stopping recording");

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
                    warn!(%error, "User recording encoder failed");
                }
                Err(error) => {
                    tracing::error!(%error, "User recording encoder task failed");
                }
            }
        }

        let mut manifest = session.manifest;
        manifest.status = ManifestStatus::Complete;
        manifest.ended_at = Some(Local::now());
        manifest.participants = participants;
        manifest.save_atomically(&session.manifest_path)?;

        tracing::info!(
            path = %session.manifest_path.display(),
            "Recording manifest written"
        );

        info!(%guild_id, session = %session_name, "Recording stopped");
        Ok(true)
    }

    pub async fn is_recording(&self) -> bool {
        self.recording_session.lock().await.is_some()
    }

    fn initiate_user_recording(
        guild_id: GuildId,
        user_id: UserId,
        started_at: DateTime<Local>,
        session_name: &str,
        initial_silence_ticks: u64,
    ) -> UserRecording {
        let (producer, consumer) = RingBuffer::<i16>::new(RING_BUFFER_CAPACITY);

        let (stop_tx, stop_rx) = oneshot::channel();

        let path = recording_path(guild_id, user_id, session_name, started_at);

        let encoder = tokio::task::spawn_blocking(move || {
            run_encoder(user_id, &path, consumer, stop_rx, initial_silence_ticks)
        });

        UserRecording {
            producer,
            stop_tx,
            encoder,
        }
    }

    pub async fn attach_to_call(&self, call: &Arc<Mutex<Call>>) -> Result<(), Error> {
        debug!("Attaching recorder event handler to voice call");
        let mut call_lock = call.lock().await;

        call_lock.add_global_event(CoreEvent::SpeakingStateUpdate.into(), self.clone());

        call_lock.add_global_event(CoreEvent::VoiceTick.into(), self.clone());

        Ok(())
    }

    pub async fn recording_info(&self) -> Option<(ChannelId, ChannelId, UserId)> {
        let recording = self.recording_session.lock().await;

        recording.as_ref().map(|session| {
            (
                session.voice_channel_id,
                session.notification_channel_id,
                session.initiator,
            )
        })
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

                let session = recording.as_mut()?;

                // Create recordings for users who have just started speaking.
                let mut manifest_changed = false;
                for &user_id in tick_audio.keys() {
                    if session.users.contains_key(&user_id) {
                        continue;
                    }

                    let user_recording = Self::initiate_user_recording(
                        session.guild_id,
                        user_id,
                        session.started_at,
                        &session.session_name,
                        session.tick,
                    );

                    session.users.insert(user_id, user_recording);
                    session.manifest.participants.push(user_id);
                    manifest_changed = true;
                }

                if manifest_changed {
                    if let Err(error) = session.manifest.save_atomically(&session.manifest_path) {
                        tracing::error!(
                            %error,
                            path = %session.manifest_path.display(),
                            "Failed to persist recording manifest after participant discovery"
                        );
                    }
                }

                // Every user gets exactly one 20 ms PCM frame per VoiceTick.
                //
                // If Songbird supplied audio, write that audio.
                // Otherwise, write 20 ms of silence.
                for (&user_id, user_recording) in &mut session.users {
                    let audio = tick_audio.get(&user_id).copied().unwrap_or(&SILENCE_FRAME);

                    write_pcm(&mut user_recording.producer, audio, user_id);
                }

                // Advance our recording timeline by one 20 ms tick.
                session.tick += 1;
            }

            _ => {}
        }

        None
    }
}

fn write_pcm(producer: &mut Producer<i16>, samples: &[i16], user_id: UserId) {
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
        "{RECORDINGS_DIR}/{}/{}-{}",
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
        .join(format!("recording-{user_id}.opus"))
}

fn ensure_recording_directory(
    guild_id: GuildId,
    session_name: &str,
    started_at: DateTime<Local>,
) -> Result<(), std::io::Error> {
    std::fs::create_dir_all(recording_directory(guild_id, session_name, started_at))
}

/// Report manifests left in the active state by a previous process lifetime.
///
/// This deliberately reports problems instead of deleting or repairing them;
/// an administrator should decide whether the corresponding audio is useful.
pub fn scan_incomplete_manifests(root: impl AsRef<Path>) -> anyhow::Result<()> {
    let root = root.as_ref();
    if !root.exists() {
        return Ok(());
    }

    scan_manifest_directory(root)
}

fn scan_manifest_directory(directory: &Path) -> anyhow::Result<()> {
    let entries = std::fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
    let has_manifest = entries.iter().any(|entry| {
        entry.path().file_name().and_then(|name| name.to_str()) == Some("manifest.toml")
    });
    let has_recording = entries.iter().any(|entry| {
        entry
            .path()
            .extension()
            .and_then(|extension| extension.to_str())
            == Some("opus")
    });

    if has_recording && !has_manifest {
        tracing::warn!(
            path = %directory.display(),
            "Found recordings without a manifest; manual cleanup or recovery is required"
        );
    }

    for entry in entries {
        let path = entry.path();

        if path.is_dir() {
            scan_manifest_directory(&path)?;
            continue;
        }

        if path.file_name().and_then(|name| name.to_str()) != Some("manifest.toml") {
            continue;
        }

        match RecordingManifest::load(&path) {
            Ok(manifest) if manifest.status == ManifestStatus::Recording => {
                tracing::warn!(
                    path = %path.display(),
                    guild_id = %manifest.guild_id,
                    started_at = %manifest.started_at,
                    participant_count = manifest.participants.len(),
                    "Found an incomplete recording manifest; manual cleanup or recovery is required"
                );
            }
            Ok(_) => {}
            Err(error) => {
                tracing::error!(
                    path = %path.display(),
                    %error,
                    "Found an unreadable recording manifest; manual cleanup or recovery is required"
                );
            }
        }
    }

    Ok(())
}

pub async fn notify_recording_user(
    http: &Http,
    channel_id: ChannelId,
    user_id: UserId,
) -> Result<(), Error> {
    channel_id
        .say(
            http,
            format!(
                "Recording notice: <@{user_id}>, this voice channel is currently being recorded. Your voice will be included in the recording."
            ),
        )
        .await?;

    Ok(())
}
