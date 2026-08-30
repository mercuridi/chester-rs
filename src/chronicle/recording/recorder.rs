use std::{
    collections::{HashMap, hash_map::Entry},
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
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
    chronicle::recording::constants::{RING_BUFFER_CAPACITY, SILENCE_FRAME},
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

#[derive(Debug, Serialize, Deserialize)]
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
    pub session_slug: String,
    pub manifest_path: PathBuf,
    pub manifest: RecordingManifest,
    pub started_instant: Instant,
    pub tick: u64,
    pub users: HashMap<UserId, UserRecording>,
}
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
}

#[derive(Clone)]
pub struct Recorder {
    pub id: u64,
    pub ssrc_to_user: Arc<Mutex<HashMap<u32, UserId>>>,
    pub recording_session: Arc<Mutex<Option<RecordingSession>>>,
    recordings_dir: PathBuf,
    clock: Arc<dyn Clock>,
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
        }
    }

    pub fn with_clock(recordings_dir: PathBuf, clock: Arc<dyn Clock>) -> Self {
        Self {
            id: rand::random(),
            ssrc_to_user: Arc::new(Mutex::new(HashMap::new())),
            recording_session: Arc::new(Mutex::new(None)),
            recordings_dir,
            clock,
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

        let recording_directory =
            recording_directory(&self.recordings_dir, guild_id, &session_slug, started_at);
        ensure_recording_directory(&self.recordings_dir, guild_id, &session_slug, started_at)?;

        let mut recording = self.recording_session.lock().await;

        if recording.is_some() {
            return Ok(false);
        }

        let manifest_path = recording_directory.join("manifest.toml");
        let mut manifest = RecordingManifest {
            status: ManifestStatus::Recording,
            guild_id,
            session_title,
            started_at,
            ended_at: None,
            participants: Vec::new(),
            scenes: Vec::new(),
        };

        if let Some(name) = initial_scene {
            manifest.scenes.push(SceneEvent {
                name: validate_scene_name(name)?,
                offset_ms: 0,
                submitted_at: started_at,
                sequence: 0,
            });
        }

        manifest.save_atomically(&manifest_path)?;

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
        if let Err(error) = session.manifest.save_atomically(&session.manifest_path) {
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
        manifest.ended_at = Some(self.clock.now());
        manifest.participants = participants;
        manifest.save_atomically(&session.manifest_path)?;

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

    fn initiate_user_recording(
        &self,
        guild_id: GuildId,
        user_id: UserId,
        started_at: DateTime<Local>,
        session_name: &str,
        initial_silence_ticks: u64,
    ) -> UserRecording {
        let (producer, consumer) = RingBuffer::<i16>::new(RING_BUFFER_CAPACITY);

        let (stop_tx, stop_rx) = oneshot::channel();

        let path = recording_path(
            &self.recordings_dir,
            guild_id,
            user_id,
            session_name,
            started_at,
        );

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
                    && let Err(error) = session.manifest.save_atomically(&session.manifest_path)
                {
                    tracing::error!(
                        %error,
                        path = %session.manifest_path.display(),
                        "Failed to persist recording manifest after participant discovery"
                    );
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

fn ensure_recording_directory(
    recordings_dir: &Path,
    guild_id: GuildId,
    session_name: &str,
    started_at: DateTime<Local>,
) -> Result<(), std::io::Error> {
    std::fs::create_dir_all(recording_directory(
        recordings_dir,
        guild_id,
        session_name,
        started_at,
    ))
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{
        Clock, ManifestStatus, Recorder, RecorderManager, RecordingManifest,
        default_manifest_status, recording_directory, recording_path, scan_incomplete_manifests,
        validate_scene_name, write_pcm,
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
        })
    }

    #[test]
    fn default_manifest_status_is_complete_for_legacy_manifests() {
        assert_eq!(default_manifest_status(), ManifestStatus::Complete);
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
    fn write_pcm_commits_complete_samples_and_drops_oversized_frames() -> anyhow::Result<()> {
        let (mut producer, mut consumer) = RingBuffer::<i16>::new(4);
        write_pcm(&mut producer, &[1, 2, 3], UserId::new(1));
        let chunk = consumer
            .read_chunk(3)
            .map_err(|error| anyhow::anyhow!("{error:?}"))?;
        assert_eq!(chunk.as_slices().0, &[1, 2, 3]);
        chunk.commit_all();

        write_pcm(&mut producer, &[1, 2, 3, 4, 5], UserId::new(1));
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
