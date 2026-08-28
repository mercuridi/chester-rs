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
