use std::sync::{Arc, Mutex};

use anyhow::{Result, bail};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeState {
    Idle,
    LlmLoaded,
    Inference,
    Transcription,
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

    /// Mark the internally managed LLM as loaded or unloaded.
    ///
    /// This is used by the LLM lifecycle implementation in the next phase.
    pub fn set_llm_loaded(&self, loaded: bool) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("GPU runtime state is poisoned"))?;

        if loaded {
            if *state != RuntimeState::Idle {
                bail!("Cannot load the LLM while GPU work is in progress");
            }

            *state = RuntimeState::LlmLoaded;
        } else {
            if *state != RuntimeState::LlmLoaded {
                bail!("The LLM is not loaded");
            }

            *state = RuntimeState::Idle;
        }

        Ok(())
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
            bail!("{busy_message}");
        }

        *state = operation;

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

impl Drop for GpuLease {
    fn drop(&mut self) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };

        if *state == self.operation {
            *state = self.previous_state;
        }
    }
}
