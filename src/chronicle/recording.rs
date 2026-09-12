pub(crate) use constants::OPUS_SAMPLE_RATE;
pub(crate) use recorder::{
    RecorderManager, RecordingManifest, SessionId, notify_recording_user,
    resolve_finalized_recordings, resolve_session_directory, scan_incomplete_manifests,
};

#[cfg(test)]
pub(crate) use recorder::{ManifestStatus, SceneEvent};

mod constants;
mod encoder;
mod recorder;
