mod app;
mod chronicle;
mod config;
mod database;
mod discord;
mod jester;
mod logging;
mod shutdown;

use app::cli::StartupOptions;

#[allow(clippy::print_stderr)]
fn main() {
    let options = match StartupOptions::parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{error:#}");
            std::process::exit(2);
        }
    };

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("Failed to create Tokio runtime: {error}");
            std::process::exit(1);
        }
    };

    let run_result = runtime.block_on(app::run(options));
    if let Err(error) = &run_result {
        eprintln!("Chester failed to start: {error:#}");
        tracing::error!("Chester failed to start: {error:#}");
        tracing::debug!(error = ?error, "Startup error chain");
    }

    runtime.shutdown_timeout(app::SHUTDOWN_TIMEOUT);

    if run_result.is_err() {
        std::process::exit(1);
    }
}
