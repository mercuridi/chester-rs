pub(crate) use audio::AudioSource;
pub(crate) use constants::{MODEL_ID, MODEL_REVISION, MODEL_SAMPLE_RATE, TRANSCRIPT_PAGE_LIMIT};
pub(crate) use service::{TranscribedSegment, Transcriber, TranscriptionService};
pub(crate) use transcript::{
    TranscriptDocument, TranscriptEntry, TranscriptFrontmatter, TranscriptParticipant,
};
pub(crate) use whisper::{TranscriptSegment, WhisperTranscriber};

mod audio;
mod constants;
mod service;
mod transcript;
mod whisper;
