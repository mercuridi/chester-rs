use std::{
    collections::{HashMap, hash_map::Entry},
    fmt,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use chrono::{DateTime, prelude::Local};
use futures::future::join_all;
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
    sync::{Mutex, mpsc, oneshot},
    task::JoinHandle,
};

use crate::{
    chronicle::atomic_write::write_atomic,
    chronicle::recording::constants::{
        RING_BUFFER_CAPACITY, RecordedFrame, SILENCE_FRAME, STEREO_FRAME_SAMPLES,
    },
    chronicle::recording::encoder::run_encoder,
    discord::context::Error,
};
use tracing::{debug, info, instrument, warn};

pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Local>;
}

#[derive(Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Local> {
        Local::now()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingManifest {
    #[serde(default = "default_manifest_status")]
    pub status: ManifestStatus,
    pub guild_id: GuildId,
    #[serde(default, alias = "session_name")]
    pub session_title: String,
    pub started_at: DateTime<Local>,
    #[serde(default)]
    pub ended_at: Option<DateTime<Local>>,
    pub participants: Vec<UserId>,
    #[serde(default)]
    pub scenes: Vec<SceneEvent>,
    #[serde(default)]
    pub finalization_error: Option<String>,
    #[serde(default)]
    pub participant_failures: Vec<ParticipantFailure>,
    #[serde(default)]
    pub finalized_recordings: Option<Vec<FinalizedRecording>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ParticipantFailure {
    pub participant: UserId,
    pub recording: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FinalizedRecording {
    pub participant: UserId,
    pub path: String,
    pub byte_length: u64,
}

/// A session directory name supplied by a user or Discord.
///
/// Keeping this as a single normal path component prevents the session value
/// from changing the meaning of the recordings root or guild directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionId(String);

impl SessionId {
    pub fn parse(value: impl Into<String>) -> Result<Self, String> {
        let value = value.into();
        let path = Path::new(&value);
        let mut components = path.components();

        match (components.next(), components.next()) {
            (Some(Component::Normal(_)), None) if !value.is_empty() => Ok(Self(value)),
            _ => Err("Invalid session identifier.".to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

pub fn resolve_session_directory(
    recordings_dir: &Path,
    guild_id: GuildId,
    session: &SessionId,
) -> Result<PathBuf, String> {
    let recordings_dir = recordings_dir
        .canonicalize()
        .map_err(|error| format!("Failed to access recordings directory: {error}"))?;
    let guild_dir = recordings_dir.join(guild_id.to_string());
    let canonical_guild_dir = guild_dir
        .canonicalize()
        .map_err(|_| format!("Recording guild directory not found: `{guild_id}`"))?;

    if !canonical_guild_dir.starts_with(&recordings_dir) {
        return Err("Recording guild directory is outside the recordings directory.".to_string());
    }

    let session_dir = canonical_guild_dir.join(session.as_str());
    let canonical_session_dir = session_dir
        .canonicalize()
        .map_err(|_| format!("Recording session not found: `{session}`"))?;

    if !canonical_session_dir.starts_with(&canonical_guild_dir) || !canonical_session_dir.is_dir() {
        return Err("Recording session is outside this guild.".to_string());
    }

    Ok(canonical_session_dir)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SceneEvent {
    pub name: String,
    pub offset_ms: u64,
    pub submitted_at: DateTime<Local>,
    pub sequence: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ManifestStatus {
    Recording,
    Finalizing,
    Partial,
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
        let contents = toml::to_string_pretty(self)?;
        write_atomic(path, contents.as_bytes())
    }

    pub fn is_finalized(&self) -> bool {
        self.status == ManifestStatus::Complete
            && self.ended_at.is_some()
            && self.finalized_recordings.is_some()
    }
}

const MANIFEST_PERSISTENCE_QUEUE_CAPACITY: usize = 64;

struct PersistManifest {
    manifest: RecordingManifest,
    path: PathBuf,
    completion: Option<oneshot::Sender<anyhow::Result<()>>>,
}

enum ManifestCommand {
    Persist(Box<PersistManifest>),
    Shutdown(oneshot::Sender<anyhow::Result<()>>),
}

#[derive(Clone)]
struct ManifestPersistence {
    sender: mpsc::Sender<ManifestCommand>,
}

impl ManifestPersistence {
    fn new() -> Self {
        let (sender, mut receiver) = mpsc::channel(MANIFEST_PERSISTENCE_QUEUE_CAPACITY);

        tokio::spawn(async move {
            while let Some(command) = receiver.recv().await {
                let (manifest, path, completion) = match command {
                    ManifestCommand::Persist(command) => {
                        let PersistManifest {
                            manifest,
                            path,
                            completion,
                        } = *command;
                        (manifest, path, completion)
                    }
                    ManifestCommand::Shutdown(completion) => {
                        let _ = completion.send(Ok(()));
                        break;
                    }
                };

                let result = tokio::task::spawn_blocking(move || manifest.save_atomically(path))
                    .await
                    .map_err(|error| anyhow::anyhow!("Manifest persistence task failed: {error}"))
                    .and_then(|result| result);

                if let Some(completion) = completion {
                    let _ = completion.send(result);
                } else if let Err(error) = result {
                    tracing::error!(%error, "Failed to persist recording manifest");
                }
            }
        });

        Self { sender }
    }

    async fn enqueue(&self, manifest: RecordingManifest, path: PathBuf) -> anyhow::Result<()> {
        self.sender
            .send(
                PersistManifest {
                    manifest,
                    path,
                    completion: None,
                }
                .into(),
            )
            .await
            .map_err(|_| anyhow::anyhow!("Manifest persistence worker has stopped"))
    }

    async fn persist(&self, manifest: RecordingManifest, path: PathBuf) -> anyhow::Result<()> {
        let (completion_sender, completion_receiver) = oneshot::channel();
        self.sender
            .send(
                PersistManifest {
                    manifest,
                    path,
                    completion: Some(completion_sender),
                }
                .into(),
            )
            .await
            .map_err(|_| anyhow::anyhow!("Manifest persistence worker has stopped"))?;

        completion_receiver
            .await
            .map_err(|_| anyhow::anyhow!("Manifest persistence worker has stopped"))?
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
        let (completion_sender, completion_receiver) = oneshot::channel();
        self.sender
            .send(ManifestCommand::Shutdown(completion_sender))
            .await
            .map_err(|_| anyhow::anyhow!("Manifest persistence worker has stopped"))?;

        completion_receiver
            .await
            .map_err(|_| anyhow::anyhow!("Manifest persistence worker has stopped"))?
    }
}

impl From<PersistManifest> for ManifestCommand {
    fn from(command: PersistManifest) -> Self {
        Self::Persist(Box::new(command))
    }
}

pub fn resolve_finalized_recordings(
    manifest: &RecordingManifest,
    recording_dir: &Path,
) -> Result<Vec<PathBuf>, String> {
    if !manifest.is_finalized() {
        return Err("This recording is not finalized; recover it before transcribing.".to_string());
    }

    manifest
        .finalized_recordings
        .as_ref()
        .expect("is_finalized guarantees finalized recordings")
        .iter()
        .map(|recording| {
            let path = recording_dir.join(&recording.path);
            let canonical = path
                .canonicalize()
                .map_err(|error| format!("Failed to resolve finalized recording: {error}"))?;
            if !canonical.starts_with(recording_dir) || !canonical.is_file() {
                return Err("Finalized recording is outside the recording session.".to_string());
            }
            let metadata = std::fs::metadata(&canonical)
                .map_err(|error| format!("Failed to inspect finalized recording: {error}"))?;
            if metadata.len() != recording.byte_length {
                return Err(format!(
                    "Finalized recording `{}` changed after finalization.",
                    recording.path
                ));
            }
            Ok(canonical)
        })
        .collect()
}

pub fn recover_recording_manifest(
    manifest_path: &Path,
    recording_dir: &Path,
    guild_id: GuildId,
) -> anyhow::Result<RecordingManifest> {
    let mut manifest = RecordingManifest::load(manifest_path)?;
    if manifest.guild_id != guild_id {
        anyhow::bail!("Recording manifest belongs to a different guild.");
    }
    if manifest.status == ManifestStatus::Recording {
        anyhow::bail!("The recording is still active and cannot be recovered yet.");
    }

    let mut artifacts = Vec::new();
    for entry in std::fs::read_dir(recording_dir)? {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("opus") {
            continue;
        }
        let canonical = path.canonicalize()?;
        if !canonical.starts_with(recording_dir) || !canonical.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(participant) = name
            .strip_prefix("recording-")
            .and_then(|name| name.strip_suffix(".opus"))
            .and_then(|id| id.parse::<u64>().ok())
            .map(UserId::new)
        else {
            continue;
        };
        let metadata = std::fs::metadata(&canonical)?;
        artifacts.push(FinalizedRecording {
            participant,
            path: name.to_string(),
            byte_length: metadata.len(),
        });
    }
    if artifacts.is_empty() {
        anyhow::bail!("No recoverable Opus recordings were found.");
    }

    manifest.participants = artifacts
        .iter()
        .map(|artifact| artifact.participant)
        .collect();
    manifest.participants.sort_unstable_by_key(|id| id.get());
    manifest.participants.dedup();
    manifest.finalized_recordings = Some(artifacts);
    manifest.participant_failures.clear();
    manifest.finalization_error = None;
    manifest.status = ManifestStatus::Complete;
    manifest.ended_at.get_or_insert_with(|| Local::now());
    manifest.save_atomically(manifest_path)?;
    Ok(manifest)
}

pub struct UserRecording {
    pub path: PathBuf,
    pub producer: Producer<RecordedFrame>,
    pub stop_tx: oneshot::Sender<u64>,
    pub encoder: JoinHandle<Result<(), Error>>,
}

pub struct RecordingSession {
    pub guild_id: GuildId,
    pub voice_channel_id: ChannelId,
    pub notification_channel_id: ChannelId,
    pub initiator: UserId,
    pub started_at: DateTime<Local>,
    pub session_slug: String,
    pub manifest_path: PathBuf,
    pub manifest: RecordingManifest,
    pub started_instant: Instant,
    pub tick: u64,
    pub users: HashMap<UserId, UserRecording>,
}
#[derive(Clone)]
pub struct RecorderManager {
    recorders: Arc<Mutex<HashMap<GuildId, Recorder>>>,
    recordings_dir: PathBuf,
    clock: Arc<dyn Clock>,
}

impl RecorderManager {
    pub fn new(recordings_dir: PathBuf) -> Self {
        Self::with_clock(recordings_dir, Arc::new(SystemClock))
    }

    pub fn with_clock(recordings_dir: PathBuf, clock: Arc<dyn Clock>) -> Self {
        Self {
            recorders: Arc::new(Mutex::new(HashMap::new())),
            recordings_dir,
            clock,
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
                let recorder =
                    Recorder::with_clock(self.recordings_dir.clone(), Arc::clone(&self.clock));
                entry.insert(recorder.clone());
                (recorder, true)
            }
        }
    }

    pub async fn remove(&self, guild_id: GuildId) -> Option<Recorder> {
        self.recorders.lock().await.remove(&guild_id)
    }

    pub async fn drain(&self) -> anyhow::Result<()> {
        let recorders = std::mem::take(&mut *self.recorders.lock().await);
        let results = join_all(recorders.into_values().map(|recorder| async move {
            let stop_result = recorder.stop_recording().await;
            let shutdown_result = recorder.shutdown_persistence().await;

            match (stop_result, shutdown_result) {
                (Ok(_), Ok(())) => Ok(()),
                (Err(stop_error), Ok(())) => Err(stop_error.to_string()),
                (Ok(_), Err(shutdown_error)) => Err(shutdown_error.to_string()),
                (Err(stop_error), Err(shutdown_error)) => Err(format!(
                    "{stop_error}; persistence shutdown failed: {shutdown_error}"
                )),
            }
        }))
        .await;
        let errors: Vec<String> = results.into_iter().filter_map(Result::err).collect();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "recording drain failed: {}",
                errors.join("; ")
            ))
        }
    }
}

#[derive(Clone)]
pub struct Recorder {
    pub id: u64,
    pub ssrc_to_user: Arc<Mutex<HashMap<u32, UserId>>>,
    pub recording_session: Arc<Mutex<Option<RecordingSession>>>,
    recordings_dir: PathBuf,
    clock: Arc<dyn Clock>,
    manifest_persistence: ManifestPersistence,
}

impl Recorder {
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "convenience constructor used by tests")
    )]
    pub fn new(recordings_dir: PathBuf) -> Self {
        Self {
            id: rand::random(),
            ssrc_to_user: Arc::new(Mutex::new(HashMap::new())),
            recording_session: Arc::new(Mutex::new(None)),
            recordings_dir,
            clock: Arc::new(SystemClock),
            manifest_persistence: ManifestPersistence::new(),
        }
    }

    pub fn with_clock(recordings_dir: PathBuf, clock: Arc<dyn Clock>) -> Self {
        Self {
            id: rand::random(),
            ssrc_to_user: Arc::new(Mutex::new(HashMap::new())),
            recording_session: Arc::new(Mutex::new(None)),
            recordings_dir,
            clock,
            manifest_persistence: ManifestPersistence::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    #[instrument(skip(self), fields(session = %session_title))]
    pub async fn start_recording(
        &self,
        guild_id: GuildId,
        voice_channel_id: ChannelId,
        notification_channel_id: ChannelId,
        initiator: UserId,
        session_title: String,
        session_slug: String,
        initial_scene: Option<String>,
    ) -> Result<bool, Error> {
        let started_at = self.clock.now();
        let started_instant = Instant::now();

        let mut recording = self.recording_session.lock().await;

        if recording.is_some() {
            return Ok(false);
        }

        let (session_slug, recording_directory) = allocate_recording_directory(
            &self.recordings_dir,
            guild_id,
            &session_slug,
            started_at,
        )?;

        let manifest_path = recording_directory.join("manifest.toml");
        let mut manifest = RecordingManifest {
            status: ManifestStatus::Recording,
            guild_id,
            session_title,
            started_at,
            ended_at: None,
            participants: Vec::new(),
            scenes: Vec::new(),
            finalization_error: None,
            participant_failures: Vec::new(),
            finalized_recordings: None,
        };

        if let Some(name) = initial_scene {
            manifest.scenes.push(SceneEvent {
                name: validate_scene_name(name)?,
                offset_ms: 0,
                submitted_at: started_at,
                sequence: 0,
            });
        }

        self.manifest_persistence
            .persist(manifest.clone(), manifest_path.clone())
            .await?;

        *recording = Some(RecordingSession {
            guild_id,
            voice_channel_id,
            notification_channel_id,
            initiator,
            started_at,
            session_slug,
            manifest_path,
            manifest,
            started_instant,
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

    pub async fn add_scene(&self, name: String) -> Result<SceneEvent, Error> {
        let mut recording = self.recording_session.lock().await;
        let session = recording
            .as_mut()
            .ok_or_else(|| -> Error { "There is no recording in progress.".into() })?;

        let event = SceneEvent {
            name: validate_scene_name(name)?,
            offset_ms: u64::try_from(
                session
                    .started_instant
                    .elapsed()
                    .as_millis()
                    .min(u128::from(u64::MAX)),
            )
            .unwrap_or(u64::MAX),
            submitted_at: self.clock.now(),
            sequence: session.manifest.scenes.len() as u64,
        };

        session.manifest.scenes.push(event.clone());
        if let Err(error) = self
            .manifest_persistence
            .persist(session.manifest.clone(), session.manifest_path.clone())
            .await
        {
            session.manifest.scenes.pop();
            return Err(error.into());
        }

        Ok(event)
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
        let session_slug = session.session_slug.clone();
        info!(%guild_id, session = %session_slug, participant_count = participants.len(), "Stopping recording");

        let mut manifest = session.manifest;
        manifest.status = ManifestStatus::Finalizing;
        manifest.participants = participants.clone();
        manifest.finalization_error = None;
        manifest.participant_failures.clear();
        manifest.finalized_recordings = Some(Vec::new());
        self.manifest_persistence
            .persist(manifest.clone(), session.manifest_path.clone())
            .await?;
        let final_tick = session.tick;

        let encoder_drains =
            session
                .users
                .into_iter()
                .map(|(participant, user_recording)| async move {
                    let UserRecording {
                        path,
                        producer,
                        stop_tx,
                        encoder,
                    } = user_recording;

                    // Tell the encoder that no more data should be expected.
                    let _ = stop_tx.send(final_tick);

                    // The producer must remain alive while the encoder drains the
                    // samples already committed to the ring buffer. Once the
                    // encoder has been told to stop, dropping the producer is safe.
                    drop(producer);
                    (participant, path, encoder.await)
                });
        let encoder_results = join_all(encoder_drains).await;
        let mut failures = Vec::new();
        let mut finalized_recordings = Vec::new();
        for (participant, path, result) in encoder_results {
            match result {
                Ok(Ok(())) => match std::fs::metadata(&path) {
                    Ok(metadata) => finalized_recordings.push(FinalizedRecording {
                        participant,
                        path: path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or_default()
                            .to_string(),
                        byte_length: metadata.len(),
                    }),
                    Err(error) => failures.push(ParticipantFailure {
                        participant,
                        recording: path.display().to_string(),
                        error: error.to_string(),
                    }),
                },
                Ok(Err(error)) => failures.push(ParticipantFailure {
                    participant,
                    recording: path.display().to_string(),
                    error: error.to_string(),
                }),
                Err(error) => failures.push(ParticipantFailure {
                    participant,
                    recording: path.display().to_string(),
                    error: error.to_string(),
                }),
            }
        }

        manifest.ended_at = Some(self.clock.now());
        manifest.status = if failures.is_empty() {
            ManifestStatus::Complete
        } else {
            manifest.finalization_error = Some(
                failures
                    .iter()
                    .map(|failure| format!("{}: {}", failure.participant, failure.error))
                    .collect::<Vec<_>>()
                    .join("; "),
            );
            ManifestStatus::Partial
        };
        manifest.participant_failures = failures.clone();
        manifest.finalized_recordings = Some(finalized_recordings);
        self.manifest_persistence
            .persist(manifest.clone(), session.manifest_path.clone())
            .await?;

        for failure in &failures {
            warn!(participant = %failure.participant, error = %failure.error, "User recording encoder failed");
        }

        if !failures.is_empty() {
            return Err(anyhow::anyhow!("one or more encoders failed").into());
        }

        tracing::info!(
            path = %session.manifest_path.display(),
            "Recording manifest written"
        );

        info!(%guild_id, session = %session_slug, "Recording stopped");
        Ok(true)
    }

    pub async fn is_recording(&self) -> bool {
        self.recording_session.lock().await.is_some()
    }

    async fn shutdown_persistence(&self) -> anyhow::Result<()> {
        self.manifest_persistence.shutdown().await
    }

    fn initiate_user_recording(
        &self,
        guild_id: GuildId,
        user_id: UserId,
        started_at: DateTime<Local>,
        session_name: &str,
        initial_silence_ticks: u64,
    ) -> UserRecording {
        let (producer, consumer) = RingBuffer::<RecordedFrame>::new(RING_BUFFER_CAPACITY);

        let (stop_tx, stop_rx) = oneshot::channel();

        let path = recording_path(
            &self.recordings_dir,
            guild_id,
            user_id,
            session_name,
            started_at,
        );

        let encoder_path = path.clone();
        let encoder = tokio::task::spawn_blocking(move || {
            run_encoder(
                user_id,
                &encoder_path,
                consumer,
                stop_rx,
                initial_silence_ticks,
            )
        });

        UserRecording {
            path,
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

                    let user_recording = self.initiate_user_recording(
                        session.guild_id,
                        user_id,
                        session.started_at,
                        &session.session_slug,
                        session.tick,
                    );

                    session.users.insert(user_id, user_recording);
                    session.manifest.participants.push(user_id);
                    manifest_changed = true;
                }

                if manifest_changed
                    && let Err(error) = self
                        .manifest_persistence
                        .enqueue(session.manifest.clone(), session.manifest_path.clone())
                        .await
                {
                    tracing::error!(
                        %error,
                        path = %session.manifest_path.display(),
                        "Failed to enqueue recording manifest after participant discovery"
                    );
                }

                // Every user gets exactly one 20 ms PCM frame per VoiceTick.
                //
                // If Songbird supplied audio, write that audio.
                // Otherwise, write 20 ms of silence.
                for (&user_id, user_recording) in &mut session.users {
                    let audio = tick_audio.get(&user_id).copied().unwrap_or(&SILENCE_FRAME);

                    write_pcm(&mut user_recording.producer, session.tick, audio, user_id);
                }

                // Advance our recording timeline by one 20 ms tick.
                session.tick += 1;
            }

            _ => {}
        }

        None
    }
}

fn write_pcm(producer: &mut Producer<RecordedFrame>, tick: u64, samples: &[i16], user_id: UserId) {
    if producer.slots() == 0 {
        tracing::warn!(
            ?user_id,
            tick,
            available = producer.slots(),
            "Recording ring buffer full; dropping PCM frame"
        );
        return;
    }

    let mut frame = RecordedFrame {
        tick,
        ..RecordedFrame::default()
    };
    let sample_count = samples.len().min(STEREO_FRAME_SAMPLES);
    frame.samples[..sample_count].copy_from_slice(&samples[..sample_count]);

    match producer.write_chunk(1) {
        Ok(mut chunk) => {
            let (first, second) = chunk.as_mut_slices();
            if let Some(slot) = first.first_mut() {
                *slot = frame;
            } else if let Some(slot) = second.first_mut() {
                *slot = frame;
            }

            chunk.commit_all();
        }

        Err(error) => {
            tracing::warn!(
                ?user_id,
                tick,
                ?error,
                "Failed to write PCM to recording ring buffer"
            );
        }
    }
}

fn recording_directory(
    recordings_dir: &Path,
    guild_id: GuildId,
    session_name: &str,
    started_at: DateTime<Local>,
) -> PathBuf {
    recordings_dir.join(format!(
        "{}/{}-{}",
        guild_id,
        started_at.format("%Y%m%d-%H%M%S"),
        session_name,
    ))
}

fn recording_path(
    recordings_dir: &Path,
    guild_id: GuildId,
    user_id: UserId,
    session_name: &str,
    started_at: DateTime<Local>,
) -> PathBuf {
    recording_directory(recordings_dir, guild_id, session_name, started_at)
        .join(format!("recording-{user_id}.opus"))
}

fn allocate_recording_directory(
    recordings_dir: &Path,
    guild_id: GuildId,
    session_name: &str,
    started_at: DateTime<Local>,
) -> Result<(String, PathBuf), std::io::Error> {
    let guild_directory = recordings_dir.join(guild_id.to_string());
    std::fs::create_dir_all(&guild_directory)?;

    let base_name = format!("{}-{}", started_at.format("%Y%m%d-%H%M%S"), session_name);
    for suffix in 0.. {
        let name = if suffix == 0 {
            base_name.clone()
        } else {
            format!("{base_name}-{suffix}")
        };
        let directory = guild_directory.join(&name);
        match std::fs::create_dir(&directory) {
            Ok(()) => return Ok((name, directory)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    unreachable!("session directory suffix space exhausted")
}

fn validate_scene_name(name: String) -> Result<String, Error> {
    if name.trim().is_empty() {
        return Err("Scene name cannot be empty.".into());
    }
    if name.contains(['\r', '\n']) {
        return Err("Scene name cannot contain line breaks.".into());
    }
    Ok(name)
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
            Ok(manifest) if manifest.status != ManifestStatus::Complete => {
                tracing::warn!(
                    path = %path.display(),
                    guild_id = %manifest.guild_id,
                    started_at = %manifest.started_at,
                    participant_count = manifest.participants.len(),
                    status = ?manifest.status,
                    "Found an incomplete recording manifest; recovery is required"
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{
        Clock, FinalizedRecording, ManifestStatus, RecordedFrame, Recorder, RecorderManager,
        RecordingManifest, allocate_recording_directory, default_manifest_status,
        recording_directory, recording_path, recover_recording_manifest,
        resolve_finalized_recordings, scan_incomplete_manifests, validate_scene_name, write_pcm,
    };
    use chrono::{DateTime, Local, TimeZone};
    use rtrb::RingBuffer;
    use serenity::model::id::{ChannelId, GuildId, UserId};
    use std::{fs, sync::Arc};
    use tempfile::tempdir;

    struct FixedClock(DateTime<Local>);

    impl Clock for FixedClock {
        fn now(&self) -> DateTime<Local> {
            self.0
        }
    }

    fn fixed_time() -> anyhow::Result<DateTime<Local>> {
        Local
            .with_ymd_and_hms(2024, 1, 2, 3, 4, 5)
            .single()
            .ok_or_else(|| anyhow::anyhow!("fixed local time is ambiguous"))
    }

    fn manifest() -> anyhow::Result<RecordingManifest> {
        Ok(RecordingManifest {
            status: ManifestStatus::Recording,
            guild_id: GuildId::new(10),
            session_title: "Session".into(),
            started_at: fixed_time()?,
            ended_at: None,
            participants: vec![UserId::new(20)],
            scenes: Vec::new(),
            finalization_error: None,
            participant_failures: Vec::new(),
            finalized_recordings: None,
        })
    }

    #[test]
    fn default_manifest_status_is_complete_for_legacy_manifests() {
        assert_eq!(default_manifest_status(), ManifestStatus::Complete);
    }

    #[test]
    fn finalized_recordings_require_complete_manifest_and_unchanged_files() -> anyhow::Result<()> {
        let directory = tempdir()?;
        let path = directory.path().join("recording-20.opus");
        fs::write(&path, [1, 2, 3])?;
        let mut manifest = manifest()?;
        manifest.status = ManifestStatus::Complete;
        manifest.ended_at = Some(fixed_time()?);
        manifest.finalized_recordings = Some(vec![FinalizedRecording {
            participant: UserId::new(20),
            path: "recording-20.opus".into(),
            byte_length: 3,
        }]);

        assert_eq!(
            resolve_finalized_recordings(&manifest, directory.path())
                .map_err(anyhow::Error::msg)?
                .len(),
            1
        );
        fs::write(&path, [1, 2])?;
        assert!(resolve_finalized_recordings(&manifest, directory.path()).is_err());
        Ok(())
    }

    #[test]
    fn recovery_rejects_active_manifest() -> anyhow::Result<()> {
        let directory = tempdir()?;
        let manifest_path = directory.path().join("manifest.toml");
        manifest()?.save_atomically(&manifest_path)?;
        assert!(
            recover_recording_manifest(&manifest_path, directory.path(), GuildId::new(10)).is_err()
        );
        Ok(())
    }

    #[test]
    fn manifest_round_trips_atomically() -> anyhow::Result<()> {
        let directory = tempdir()?;
        let path = directory.path().join("manifest.toml");
        let expected = manifest()?;
        expected.save_atomically(&path)?;
        let actual = RecordingManifest::load(&path)?;

        assert_eq!(actual.status, ManifestStatus::Recording);
        assert_eq!(actual.guild_id, GuildId::new(10));
        assert_eq!(actual.session_title, "Session");
        assert_eq!(actual.participants, vec![UserId::new(20)]);
        assert!(!path.with_extension("toml.tmp").exists());
        Ok(())
    }

    #[test]
    fn legacy_manifest_defaults_status_and_accepts_session_name() -> anyhow::Result<()> {
        let source = format!(
            "guild_id = 10\nsession_name = \"Legacy\"\nstarted_at = {}\nparticipants = []\n",
            toml::Value::String(fixed_time()?.to_rfc3339())
        );
        let parsed: RecordingManifest = toml::from_str(&source)?;
        assert_eq!(parsed.status, ManifestStatus::Complete);
        assert_eq!(parsed.session_title, "Legacy");
        assert!(parsed.scenes.is_empty());
        assert!(parsed.ended_at.is_none());
        Ok(())
    }

    #[test]
    fn recording_paths_include_guild_timestamp_session_and_user() -> anyhow::Result<()> {
        let root = std::path::Path::new("recordings");
        let directory = recording_directory(root, GuildId::new(10), "session", fixed_time()?);
        assert_eq!(directory, root.join("10/20240102-030405-session"));
        assert_eq!(
            recording_path(
                root,
                GuildId::new(10),
                UserId::new(20),
                "session",
                fixed_time()?
            ),
            directory.join("recording-20.opus")
        );
        Ok(())
    }

    #[test]
    fn recording_directory_allocation_does_not_reuse_colliding_session() -> anyhow::Result<()> {
        let directory = tempdir()?;
        let first = allocate_recording_directory(
            directory.path(),
            GuildId::new(10),
            "session",
            fixed_time()?,
        )?;
        let second = allocate_recording_directory(
            directory.path(),
            GuildId::new(10),
            "session",
            fixed_time()?,
        )?;

        assert_eq!(first.0, "20240102-030405-session");
        assert_eq!(second.0, "20240102-030405-session-1");
        assert!(first.1.is_dir());
        assert!(second.1.is_dir());
        assert_ne!(first.1, second.1);
        Ok(())
    }

    #[test]
    fn validates_scene_names_without_modifying_valid_input() -> anyhow::Result<()> {
        let valid = validate_scene_name(" Scene ".into())
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        assert_eq!(valid, " Scene ");
        for invalid in ["", "   ", "line\nbreak", "line\rbreak"] {
            assert!(validate_scene_name(invalid.into()).is_err(), "{invalid:?}");
        }
        Ok(())
    }

    #[test]
    fn write_pcm_commits_timestamped_frames_and_drops_when_full() -> anyhow::Result<()> {
        let (mut producer, mut consumer) = RingBuffer::<RecordedFrame>::new(1);
        write_pcm(&mut producer, 7, &[1, 2, 3], UserId::new(1));
        let chunk = consumer
            .read_chunk(1)
            .map_err(|error| anyhow::anyhow!("{error:?}"))?;
        let frame = &chunk.as_slices().0[0];
        assert_eq!(frame.tick, 7);
        assert_eq!(&frame.samples[..3], &[1, 2, 3]);
        assert!(frame.samples[3..].iter().all(|sample| *sample == 0));
        chunk.commit_all();

        write_pcm(&mut producer, 8, &[4, 5], UserId::new(1));
        write_pcm(&mut producer, 9, &[6, 7], UserId::new(1));
        let frame = consumer
            .read_chunk(1)
            .map_err(|error| anyhow::anyhow!("{error:?}"))?;
        assert_eq!(frame.as_slices().0[0].tick, 8);
        frame.commit_all();
        assert!(consumer.read_chunk(1).is_err());
        Ok(())
    }

    #[tokio::test]
    async fn manager_reuses_and_removes_recorders_by_guild() -> anyhow::Result<()> {
        let directory = tempdir()?;
        let manager = RecorderManager::with_clock(
            directory.path().into(),
            Arc::new(FixedClock(fixed_time()?)),
        );
        let (first, created) = manager.get_or_create(GuildId::new(1)).await;
        assert!(created);
        let (second, created) = manager.get_or_create(GuildId::new(1)).await;
        assert!(!created);
        assert_eq!(first.id, second.id);
        assert_eq!(
            manager.get(GuildId::new(1)).await.map(|item| item.id),
            Some(first.id)
        );
        assert_eq!(
            manager.remove(GuildId::new(1)).await.map(|item| item.id),
            Some(first.id)
        );
        assert!(manager.get(GuildId::new(1)).await.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn manager_drain_flushes_and_shuts_down_manifest_persistence() -> anyhow::Result<()> {
        let directory = tempdir()?;
        let manager = RecorderManager::with_clock(
            directory.path().into(),
            Arc::new(FixedClock(fixed_time()?)),
        );
        let (recorder, created) = manager.get_or_create(GuildId::new(1)).await;
        assert!(created);

        recorder
            .start_recording(
                GuildId::new(1),
                ChannelId::new(2),
                ChannelId::new(3),
                UserId::new(4),
                "Title".into(),
                "slug".into(),
                None,
            )
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;

        manager.drain().await?;

        let path = recording_directory(directory.path(), GuildId::new(1), "slug", fixed_time()?)
            .join("manifest.toml");
        assert_eq!(
            RecordingManifest::load(path)?.status,
            ManifestStatus::Complete
        );
        Ok(())
    }

    #[tokio::test]
    async fn recorder_lifecycle_persists_manifest_and_scene() -> anyhow::Result<()> {
        let directory = tempdir()?;
        let recorder =
            Recorder::with_clock(directory.path().into(), Arc::new(FixedClock(fixed_time()?)));
        let started = recorder
            .start_recording(
                GuildId::new(1),
                ChannelId::new(2),
                ChannelId::new(3),
                UserId::new(4),
                "Title".into(),
                "slug".into(),
                Some("Opening".into()),
            )
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        assert!(started);
        assert!(recorder.is_recording().await);
        assert!(
            !recorder
                .start_recording(
                    GuildId::new(1),
                    ChannelId::new(2),
                    ChannelId::new(3),
                    UserId::new(4),
                    "Other".into(),
                    "other".into(),
                    None
                )
                .await
                .map_err(|error| anyhow::anyhow!(error.to_string()))?
        );
        assert_eq!(
            recorder.recording_info().await,
            Some((ChannelId::new(2), ChannelId::new(3), UserId::new(4)))
        );

        let scene = recorder
            .add_scene("Second".into())
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        assert_eq!(scene.sequence, 1);
        assert_eq!(scene.name, "Second");
        assert!(
            recorder
                .stop_recording()
                .await
                .map_err(|error| anyhow::anyhow!(error.to_string()))?
        );
        assert!(
            !recorder
                .stop_recording()
                .await
                .map_err(|error| anyhow::anyhow!(error.to_string()))?
        );
        assert!(!recorder.is_recording().await);

        let path = recording_directory(directory.path(), GuildId::new(1), "slug", fixed_time()?)
            .join("manifest.toml");
        let saved = RecordingManifest::load(path)?;
        assert_eq!(saved.status, ManifestStatus::Complete);
        assert_eq!(saved.ended_at, Some(fixed_time()?));
        assert_eq!(saved.scenes.len(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn adding_scene_without_recording_fails() -> anyhow::Result<()> {
        let directory = tempdir()?;
        let recorder = Recorder::new(directory.path().into());
        let error = recorder.add_scene("Scene".into()).await.unwrap_err();
        assert!(error.to_string().contains("no recording"));
        Ok(())
    }

    #[test]
    fn incomplete_manifest_scan_accepts_missing_empty_and_nested_directories() -> anyhow::Result<()>
    {
        let directory = tempdir()?;
        scan_incomplete_manifests(directory.path().join("missing"))?;
        fs::create_dir(directory.path().join("nested"))?;
        fs::write(directory.path().join("nested/recording-1.opus"), [])?;
        scan_incomplete_manifests(directory.path())?;
        Ok(())
    }
}
