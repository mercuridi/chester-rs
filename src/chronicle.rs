mod atomic_write;
pub(crate) mod config;
mod eval;
pub(crate) mod indexer;
mod llm;
mod query;
pub(crate) mod recording;
mod runtime;
mod service;
mod synthesis;
mod synthesis_eval;
pub(crate) mod transcription;

pub(crate) use config::{AppPaths, Config};
pub(crate) use eval::run as run_eval;
pub(crate) use indexer::{
    db::IndexerDb, embedder::Embedder, retriever::Retriever, service::Indexer,
};
pub(crate) use llm::Llm;
pub(crate) use query::{run as run_query, run_planner};
pub(crate) use recording::{RecorderManager, notify_recording_user, scan_incomplete_manifests};
pub(crate) use runtime::{GpuRuntime, report_cuda_oom};
pub(crate) use service::{Chronicle, ChronicleDependencies};
pub(crate) use synthesis_eval::run as run_synthesis_eval;
pub(crate) use transcription::TranscriptionService;
