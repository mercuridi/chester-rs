use std::path::Path;

use anyhow::Result;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

const DEFAULT_LOG_LEVEL: &str = "info";
const DEFAULT_LOG_FILTER: &str = "chester_rs=info,warn";

pub fn initialize(env_path: &Path, log_dir: &Path) -> Result<crate::logging::LogWriter> {
    let settings = LoggingSettings::load(env_path);
    let log_writer = crate::logging::build_writer(log_dir, settings.log_level)?;
    let terminal_layer = tracing_subscriber::fmt::layer();
    let file_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(log_writer.writer());

    tracing_subscriber::registry()
        .with(settings.filter)
        .with(terminal_layer)
        .with(file_layer)
        .try_init()
        .map_err(|error| anyhow::anyhow!("Failed to initialize logging: {error}"))?;

    if let Some(error) = settings.invalid_filter {
        tracing::warn!(?error, "Invalid RUST_LOG filter; using the default filter");
    }

    Ok(log_writer)
}

fn configured_log_level(settings: &str) -> &'static str {
    let settings = settings.to_lowercase();
    ["trace", "debug", "info", "warn", "error"]
        .into_iter()
        .find(|level| {
            settings
                .split(|c: char| !c.is_ascii_alphabetic())
                .any(|part| part == *level)
        })
        .unwrap_or(DEFAULT_LOG_LEVEL)
}

struct LoggingSettings {
    filter: EnvFilter,
    log_level: &'static str,
    invalid_filter: Option<tracing_subscriber::filter::ParseError>,
}

impl LoggingSettings {
    fn load(env_path: &Path) -> Self {
        let directives = selected_log_directive(
            dotenv_log_directive(env_path),
            std::env::var("RUST_LOG").ok(),
        );
        match EnvFilter::try_new(&directives) {
            Ok(filter) => Self {
                filter,
                log_level: configured_log_level(&directives),
                invalid_filter: None,
            },
            Err(error) => Self {
                filter: EnvFilter::new(DEFAULT_LOG_FILTER),
                log_level: configured_log_level(DEFAULT_LOG_FILTER),
                invalid_filter: Some(error),
            },
        }
    }
}

fn selected_log_directive(
    dotenv_directive: Option<String>,
    inherited_directive: Option<String>,
) -> String {
    dotenv_directive
        .or(inherited_directive)
        .unwrap_or_else(|| DEFAULT_LOG_FILTER.into())
}

fn dotenv_log_directive(env_path: &Path) -> Option<String> {
    #[allow(deprecated)]
    let entries = dotenv::from_path_iter(env_path).ok()?;
    entries
        .filter_map(Result::ok)
        .fold(None, |directive, (key, value)| {
            (key == "RUST_LOG").then_some(value).or(directive)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_logfile_level_from_filter_directives() {
        assert_eq!(configured_log_level("chester_rs=debug,warn"), "debug");
        assert_eq!(configured_log_level("invalid-directive"), "info");
    }

    #[test]
    fn dotenv_log_directive_takes_precedence_over_the_inherited_value() {
        assert_eq!(
            selected_log_directive(Some("chester_rs=debug".into()), Some("warn".into())),
            "chester_rs=debug"
        );
    }
}
