mod bot;
pub mod cli;
mod evaluation;
mod logging;
mod startup;

use anyhow::{Context, Result};
use dotenv::from_path;
use std::time::Duration;

use crate::config::AppPaths;

pub const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(20);

pub async fn run(options: cli::StartupOptions) -> Result<()> {
    let paths = AppPaths::from_runtime_root(&options.runtime_root, options.config_path.as_deref())
        .context("Failed to resolve runtime paths")?;

    // Credentials and other application configuration remain available from
    // .env. This is deliberately completed before application startup.
    from_path(&paths.env_path).ok();

    let _log_guard = logging::initialize(&paths.env_path, &paths.log_dir)?;

    match options.invocation {
        cli::Invocation::Bot => bot::run(paths, SHUTDOWN_TIMEOUT).await,
        cli::Invocation::Evaluation(command) => evaluation::run(command, paths).await,
    }
}
