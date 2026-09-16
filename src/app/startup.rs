use std::fmt;
use std::future::Future;

use anyhow::{Error, Result};

/// Errors collected while attempting the independent startup stages.
#[derive(Default)]
pub(crate) struct StartupErrors {
    errors: Vec<(String, Error)>,
}

impl StartupErrors {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn push(&mut self, stage: impl Into<String>, error: Error) {
        self.errors.push((stage.into(), error));
    }

    pub(crate) fn push_skipped(&mut self, stage: impl Into<String>, reason: impl Into<String>) {
        self.push(stage, anyhow::anyhow!("stage skipped: {}", reason.into()));
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.errors.is_empty()
    }

    pub(crate) fn len(&self) -> usize {
        self.errors.len()
    }

    pub(crate) fn into_error(self) -> Error {
        Error::new(self)
    }
}

pub(crate) async fn run_independent_stages<T, U, F, G>(
    first_stage: &str,
    first: F,
    second_stage: &str,
    second: G,
) -> (Option<T>, Option<U>, StartupErrors)
where
    F: Future<Output = Result<T>>,
    G: Future<Output = Result<U>>,
{
    let (first_result, second_result) = tokio::join!(first, second);
    let mut errors = StartupErrors::new();
    let first = record_stage_result(first_stage, first_result, &mut errors);
    let second = record_stage_result(second_stage, second_result, &mut errors);
    (first, second, errors)
}

fn record_stage_result<T>(stage: &str, result: Result<T>, errors: &mut StartupErrors) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) => {
            errors.push(stage, error);
            None
        }
    }
}

pub(crate) async fn run_if_ready<T, F, G>(
    errors: StartupErrors,
    prepared: Option<T>,
    run_loop: F,
) -> Result<()>
where
    F: FnOnce(T) -> G,
    G: Future<Output = Result<()>>,
{
    if !errors.is_empty() {
        return Err(errors.into_error());
    }

    let Some(prepared) = prepared else {
        return Err(anyhow::anyhow!(
            "Startup completed without a prepared runtime"
        ));
    };
    run_loop(prepared).await
}

impl fmt::Display for StartupErrors {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "{} startup stage(s) failed:", self.len())?;
        for (index, (stage, error)) in self.errors.iter().enumerate() {
            writeln!(formatter, "{}. {stage}: {error:#}", index + 1)?;
        }
        Ok(())
    }
}

impl fmt::Debug for StartupErrors {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StartupErrors")
            .field("errors", &self.errors)
            .finish()
    }
}

impl std::error::Error for StartupErrors {}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use super::*;

    #[tokio::test]
    async fn independent_stage_failure_does_not_prevent_the_other_stage() {
        let second_ran = Arc::new(AtomicBool::new(false));
        let second_ran_in_stage = Arc::clone(&second_ran);
        let (first, second, errors) = run_independent_stages(
            "first",
            async { Err::<(), _>(anyhow::anyhow!("first failed")) },
            "second",
            async move {
                second_ran_in_stage.store(true, Ordering::Relaxed);
                Ok::<_, Error>("second succeeded")
            },
        )
        .await;

        assert!(first.is_none());
        assert_eq!(second, Some("second succeeded"));
        assert!(second_ran.load(Ordering::Relaxed));
        assert!(errors.to_string().contains("first: first failed"));
    }

    #[test]
    fn formats_all_errors_with_stage_names() {
        let mut errors = StartupErrors::new();
        errors.push("configuration", anyhow::anyhow!("config file is missing"));
        errors.push("Discord token", anyhow::anyhow!("token is not set"));

        let rendered = errors.to_string();

        assert_eq!(errors.len(), 2);
        assert!(!errors.is_empty());
        assert!(rendered.contains("2 startup stage(s) failed:"));
        assert!(rendered.contains("1. configuration: config file is missing"));
        assert!(rendered.contains("2. Discord token: token is not set"));
    }

    #[test]
    fn preserves_the_full_error_chain_in_rendered_output() {
        let error = anyhow::anyhow!("permission denied")
            .context("could not open log directory")
            .context("logging initialization failed");
        let mut errors = StartupErrors::new();
        errors.push("logging", error);

        let rendered = errors.to_string();

        assert!(rendered.contains(
            "1. logging: logging initialization failed: could not open log directory: permission denied"
        ));
    }

    #[test]
    fn converts_to_an_anyhow_error_without_losing_the_report() {
        let mut errors = StartupErrors::new();
        errors.push("Chronicle", anyhow::anyhow!("model unavailable"));

        let error = errors.into_error();

        assert!(format!("{error:#}").contains("Chronicle: model unavailable"));
    }

    #[test]
    fn formats_skipped_stages_as_startup_diagnostics() {
        let mut errors = StartupErrors::new();
        errors.push_skipped("audio synchronization", "the database is unavailable");

        assert!(
            errors
                .to_string()
                .contains("audio synchronization: stage skipped: the database is unavailable")
        );
    }

    #[tokio::test]
    async fn startup_errors_prevent_the_main_loop_from_running() {
        let loop_ran = Arc::new(AtomicBool::new(false));
        let loop_ran_in_loop = Arc::clone(&loop_ran);
        let mut errors = StartupErrors::new();
        errors.push("Chronicle", anyhow::anyhow!("model unavailable"));

        let result = run_if_ready(errors, Some(()), move |()| async move {
            loop_ran_in_loop.store(true, Ordering::Relaxed);
            Ok(())
        })
        .await;

        assert!(result.is_err());
        assert!(!loop_ran.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn a_ready_startup_reaches_the_main_loop() -> Result<()> {
        let loop_ran = Arc::new(AtomicBool::new(false));
        let loop_ran_in_loop = Arc::clone(&loop_ran);

        run_if_ready(StartupErrors::new(), Some(()), move |()| async move {
            loop_ran_in_loop.store(true, Ordering::Relaxed);
            Ok(())
        })
        .await?;

        assert!(loop_ran.load(Ordering::Relaxed));
        Ok(())
    }
}
