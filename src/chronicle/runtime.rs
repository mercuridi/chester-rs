use std::sync::{Arc, Mutex};

use anyhow::{Result, bail};
use tracing::{debug, info, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeState {
    Idle,
    LoadingLlm,
    LlmLoaded,
    Inference,
    Transcription,
    UnloadingLlm,
}

#[derive(Clone)]
pub struct GpuRuntime {
    state: Arc<Mutex<RuntimeState>>,
}

impl GpuRuntime {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(RuntimeState::Idle)),
        }
    }

    pub fn begin_llm_load(&self) -> Result<GpuLease> {
        self.acquire(
            RuntimeState::Idle,
            RuntimeState::LoadingLlm,
            "Cannot load the LLM while GPU work is in progress",
        )
    }

    pub fn begin_llm_unload(&self) -> Result<GpuLease> {
        self.acquire(
            RuntimeState::LlmLoaded,
            RuntimeState::UnloadingLlm,
            "Cannot unload the LLM while an operation is running",
        )
    }

    pub fn is_llm_loaded(&self) -> Result<bool> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("GPU runtime state is poisoned"))?;

        Ok(matches!(
            *state,
            RuntimeState::LlmLoaded | RuntimeState::Inference
        ))
    }

    /// Acquire exclusive GPU access for an LLM inference.
    pub fn acquire_inference(&self) -> Result<GpuLease> {
        self.acquire(
            RuntimeState::LlmLoaded,
            RuntimeState::Inference,
            "LLM inference is unavailable until the LLM is loaded",
        )
    }

    /// Acquire exclusive GPU access for Whisper transcription.
    pub fn acquire_transcription(&self) -> Result<GpuLease> {
        self.acquire(
            RuntimeState::Idle,
            RuntimeState::Transcription,
            "Transcription is unavailable while the LLM is loaded or another GPU operation is running",
        )
    }

    fn acquire(
        &self,
        required_state: RuntimeState,
        operation: RuntimeState,
        busy_message: &'static str,
    ) -> Result<GpuLease> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("GPU runtime state is poisoned"))?;

        if *state != required_state {
            warn!(
                ?state,
                ?required_state,
                ?operation,
                "GPU operation rejected because runtime is busy"
            );
            bail!("{busy_message}");
        }

        *state = operation;
        debug!(?operation, "Acquired GPU runtime lease");

        Ok(GpuLease {
            state: Arc::clone(&self.state),
            previous_state: required_state,
            operation,
        })
    }
}

impl Default for GpuRuntime {
    fn default() -> Self {
        Self::new()
    }
}

pub struct GpuLease {
    state: Arc<Mutex<RuntimeState>>,
    previous_state: RuntimeState,
    operation: RuntimeState,
}

impl GpuLease {
    pub fn commit_to_loaded(self) -> Result<()> {
        let result = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("GPU runtime state is poisoned"))
            .and_then(|mut state| {
                if *state != self.operation {
                    bail!("GPU runtime state changed unexpectedly");
                }

                *state = RuntimeState::LlmLoaded;
                info!("GPU runtime transitioned to LLM loaded");
                Ok(())
            });

        if result.is_ok() {
            std::mem::forget(self);
        }
        result
    }

    pub fn commit_to_idle(self) -> Result<()> {
        let result = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("GPU runtime state is poisoned"))
            .and_then(|mut state| {
                if *state != self.operation {
                    bail!("GPU runtime state changed unexpectedly");
                }

                *state = RuntimeState::Idle;
                info!("GPU runtime transitioned to idle");
                Ok(())
            });

        if result.is_ok() {
            std::mem::forget(self);
        }
        result
    }
}

impl Drop for GpuLease {
    fn drop(&mut self) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };

        if *state == self.operation {
            *state = self.previous_state;
            debug!(operation = ?self.operation, "Released GPU runtime lease");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::GpuRuntime;

    #[test]
    fn runtime_starts_idle() -> anyhow::Result<()> {
        let runtime = GpuRuntime::new();
        assert!(!runtime.is_llm_loaded()?);
        Ok(())
    }

    #[test]
    fn loading_lease_rolls_back_to_idle_when_dropped() -> anyhow::Result<()> {
        let runtime = GpuRuntime::new();
        let lease = runtime.begin_llm_load()?;
        assert!(runtime.begin_llm_load().is_err());
        assert!(runtime.acquire_transcription().is_err());
        drop(lease);
        assert!(runtime.acquire_transcription().is_ok());
        Ok(())
    }

    #[test]
    fn loading_can_commit_to_loaded() -> anyhow::Result<()> {
        let runtime = GpuRuntime::new();
        runtime.begin_llm_load()?.commit_to_loaded()?;
        assert!(runtime.is_llm_loaded()?);
        assert!(runtime.acquire_transcription().is_err());
        Ok(())
    }

    #[test]
    fn inference_requires_loaded_state_and_restores_it_on_drop() -> anyhow::Result<()> {
        let runtime = GpuRuntime::new();
        assert!(runtime.acquire_inference().is_err());
        runtime.begin_llm_load()?.commit_to_loaded()?;
        let lease = runtime.acquire_inference()?;
        assert!(runtime.is_llm_loaded()?);
        assert!(runtime.acquire_inference().is_err());
        drop(lease);
        assert!(runtime.acquire_inference().is_ok());
        Ok(())
    }

    #[test]
    fn unload_requires_loaded_state_and_can_commit_to_idle() -> anyhow::Result<()> {
        let runtime = GpuRuntime::new();
        assert!(runtime.begin_llm_unload().is_err());
        runtime.begin_llm_load()?.commit_to_loaded()?;
        runtime.begin_llm_unload()?.commit_to_idle()?;
        assert!(!runtime.is_llm_loaded()?);
        assert!(runtime.acquire_transcription().is_ok());
        Ok(())
    }

    #[test]
    fn unload_lease_rolls_back_to_loaded_when_dropped() -> anyhow::Result<()> {
        let runtime = GpuRuntime::new();
        runtime.begin_llm_load()?.commit_to_loaded()?;
        let lease = runtime.begin_llm_unload()?;
        assert!(!runtime.is_llm_loaded()?);
        drop(lease);
        assert!(runtime.is_llm_loaded()?);
        Ok(())
    }

    #[test]
    fn transcription_is_exclusive_and_restores_idle() -> anyhow::Result<()> {
        let runtime = GpuRuntime::new();
        let lease = runtime.acquire_transcription()?;
        assert!(runtime.acquire_transcription().is_err());
        assert!(runtime.begin_llm_load().is_err());
        drop(lease);
        assert!(runtime.begin_llm_load().is_ok());
        Ok(())
    }

    #[test]
    fn cloned_runtimes_share_state() -> anyhow::Result<()> {
        let runtime = GpuRuntime::new();
        let clone = runtime.clone();
        runtime.begin_llm_load()?.commit_to_loaded()?;
        assert!(clone.is_llm_loaded()?);
        assert!(clone.acquire_transcription().is_err());
        Ok(())
    }
}
