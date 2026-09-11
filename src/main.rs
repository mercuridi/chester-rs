mod chronicle;
mod database;
mod discord;
mod jester;
mod logging;
mod shutdown;
mod utils;

////////////////////////////////////////////////////////////////////////////////
use dotenv::from_path;
/// Imports
use poise::serenity_prelude::{ClientBuilder, Context as SerenityContext, GatewayIntents};
use serenity::client::FullEvent;
use songbird::{
    Config as SongbirdConfig, SerenityInit,
    driver::{DecodeConfig, DecodeMode},
};
use sqlx::SqlitePool;
use tracing::info;

use crate::{
    chronicle::{
        config::{app::Config, paths::AppPaths},
        indexer::retriever::runtime::Retriever,
        indexer::{db::repository::facade::IndexerDb, embedder::Embedder, service::Indexer},
        llm::Llm,
        recording::recorder::{notify_recording_user, scan_incomplete_manifests},
        runtime::{GpuRuntime, report_cuda_oom},
        service::{Chronicle, ChronicleDependencies},
        transcription::service::TranscriptionService,
    },
    discord::context::{Data, Error},
    jester::{
        library::sync::{SyncConfig, sync_audio_library},
        track::download::Downloader,
    },
};
use anyhow::{Context, Result, bail};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

const DEFAULT_CHRONICLE_EVAL_SUITE: &str = "tests/fixtures/chronicle/suite.toml";
const DEFAULT_CHRONICLE_QUERY_EVAL_SUITE: &str = "tests/fixtures/chronicle-query/suite.toml";
const DEFAULT_CHRONICLE_SYNTHESIS_EVAL_SUITE: &str =
    "tests/fixtures/chronicle-synthesis/suite.toml";
const DEFAULT_LOG_LEVEL: &str = "info";
const DEFAULT_LOG_FILTER: &str = "chester_rs=info,warn";

////////////////////////////////////////////////////////////////////////////////
// Functions

async fn on_error(error: poise::FrameworkError<'_, Data, Error>) {
    // 1) Inspect & log any command errors without moving out of `error`
    match &error {
        // Log setup failures before forwarding them to Poise's handler.
        poise::FrameworkError::Setup {
            error: setup_err, ..
        } => {
            tracing::error!(error = ?setup_err, "Failed to start bot");
        }
        // Log command errors
        poise::FrameworkError::Command {
            ctx,
            error: cmd_err,
            ..
        } => {
            tracing::warn!(
                command = %ctx.command().name,
                error_chain = %format!("{cmd_err:#}"),
                "Command failed"
            );
        }
        // You can match other variants here if you like...
        _ => {}
    }

    // 2) Forward the _owned_ `error` to Poise's default handler so it replies in Discord
    if let Err(e) = poise::builtins::on_error(error).await {
        tracing::error!("Error while handling error: {}", e);
    }
}

async fn build_chronicle(config: &Config) -> Result<Chronicle> {
    tracing::info!(
        chronicle_db = %config.database.chronicle,
        "Opening Chronicle index database"
    );

    let chronicle_db = IndexerDb::open(&config.database.chronicle)
        .await
        .context("Failed to open Chronicle index database")?;
    tracing::info!("Loading Chronicle embedding model");
    let device = candle_core::Device::cuda_if_available(0)
        .map_err(anyhow::Error::from)
        .inspect_err(|error| {
            report_cuda_oom(error, "embedding", "device_initialization");
        })
        .context("Failed to select a CUDA or CPU device for Chronicle embeddings")?;
    let embedder = Embedder::load(device)
        .inspect_err(|error| {
            report_cuda_oom(error, "embedding", "load");
        })
        .context("Failed to load the Chronicle embedding model")?;
    let indexer = Indexer::new(
        config.chronicle.indexing.corpus_dir.clone(),
        chronicle_db,
        embedder,
        config.chronicle.indexing.max_chunk_tokens,
        config.chronicle.indexing.chunk_overlap_tokens,
    )
    .with_excluded_note_ids(config.chronicle.indexing.excluded_note_ids.clone());

    let indexing_stats = indexer
        .index()
        .await
        .context("Failed to index the Chronicle corpus")?;
    tracing::info!(
        added = indexing_stats.added,
        updated = indexing_stats.updated,
        unchanged = indexing_stats.unchanged,
        removed = indexing_stats.removed,
        "Chronicle index complete"
    );

    let (chronicle_db, _embedder) = indexer.into_parts();
    let runtime = GpuRuntime::new();
    let llm = Arc::new(Llm::new(&config.chronicle.llm, runtime.clone()));
    let retriever = Arc::new(Retriever::new(chronicle_db.clone()));
    tracing::info!("Chronicle services initialized");

    Ok(Chronicle::new(
        config.chronicle.retrieval,
        config.chronicle.synthesis,
        config.chronicle.llm.generation.clone(),
        ChronicleDependencies {
            retriever,
            structured_store: Arc::new(chronicle_db),
            llm,
            runtime: runtime.clone(),
            transcription: TranscriptionService::new(runtime),
        },
    ))
}

fn build_commands() -> Vec<poise::Command<Data, Error>> {
    vec![
        discord::commands::admin::help(),
        discord::commands::admin::register(),
        discord::commands::controls::join(),
        discord::commands::controls::play(),
        discord::commands::controls::mix(),
        discord::commands::controls::queue(),
        discord::commands::controls::skip(),
        discord::commands::controls::history(),
        discord::commands::controls::leave(),
        discord::commands::controls::loop_track(),
        discord::commands::controls::pause(),
        discord::commands::controls::now_playing(),
        discord::commands::management::download(),
        discord::commands::management::reset_taxonomy(),
        discord::commands::management::set_taxonomy(),
        discord::commands::management::add_texture(),
        discord::commands::management::add_environment(),
        discord::commands::management::add_label(),
        discord::commands::management::set_metadata(),
        discord::commands::management::fix(),
        discord::commands::library::library(),
        discord::commands::chronicle::recording(),
        discord::commands::chronicle::transcript(),
        discord::commands::chronicle::chronicle(),
    ]
}

fn handle_event<'a>(
    ctx: &'a SerenityContext,
    event: &'a FullEvent,
    _framework: poise::FrameworkContext<'a, Data, Error>,
    data: &'a Data,
) -> poise::BoxFuture<'a, Result<(), Error>> {
    Box::pin(async move {
        if data.shutdown.is_requested() {
            return Ok(());
        }
        if let FullEvent::VoiceStateUpdate { old, new } = event {
            let Some(guild_id) = new.guild_id else {
                return Ok(());
            };

            let Some(recorder) = data.recorder.get(guild_id).await else {
                return Ok(());
            };

            let Some((voice_channel_id, notification_channel_id, initiator)) =
                recorder.recording_info().await
            else {
                return Ok(());
            };

            let user_id = new.user_id;
            tracing::debug!(
                ?guild_id,
                ?user_id,
                ?old,
                ?new,
                "Received voice state update"
            );

            // The initiator is deliberately excluded from notifications.
            if user_id == initiator {
                return Ok(());
            }

            // Only notify when the user enters the recording channel.
            if new.channel_id != Some(voice_channel_id)
                || old.as_ref().and_then(|state| state.channel_id) == Some(voice_channel_id)
            {
                return Ok(());
            }

            notify_recording_user(&ctx.http, notification_channel_id, user_id).await?;
            tracing::info!(?guild_id, ?user_id, "Notified user about active recording");
        }

        Ok(())
    })
}

fn build_framework(
    pool: SqlitePool,
    config: Config,
    chronicle: Arc<Chronicle>,
    recorder: crate::chronicle::recording::recorder::RecorderManager,
    player: Arc<jester::player::service::PlayerService>,
    shutdown: Arc<shutdown::ShutdownState>,
    downloader: Arc<Downloader>,
    poise_commands: Vec<poise::Command<Data, Error>>,
) -> poise::Framework<Data, Error> {
    let poise_options = poise::FrameworkOptions {
        commands: poise_commands,
        prefix_options: poise::PrefixFrameworkOptions {
            prefix: Some(">".into()),
            ..Default::default()
        },
        on_error: |error| Box::pin(on_error(error)),
        pre_command: |ctx| {
            Box::pin(async move {
                tracing::debug!("Executing command {}...", ctx.command().qualified_name);
            })
        },
        post_command: |ctx| {
            Box::pin(async move {
                tracing::debug!(
                    "Successfully executed command {}",
                    ctx.command().qualified_name
                );
            })
        },
        skip_checks_for_owners: true,
        event_handler: handle_event,
        ..Default::default()
    };

    poise::Framework::builder()
        .options(poise_options)
        .setup(|_ctx, _ready, _framework| {
            Box::pin(async move {
                Ok(Data::new(
                    pool, config, chronicle, recorder, player, downloader, shutdown,
                ))
            })
        })
        .build()
}

#[allow(clippy::print_stderr)]
fn main() {
    let startup = match StartupOptions::parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{error:#}");
            std::process::exit(2);
        }
    };
    let paths =
        match AppPaths::from_runtime_root(&startup.runtime_root, startup.config_path.as_deref()) {
            Ok(paths) => paths,
            Err(error) => {
                eprintln!("Failed to resolve runtime paths: {error:#}");
                std::process::exit(2);
            }
        };

    // Credentials and other application configuration remain available from
    // .env. This is deliberately completed before the Tokio runtime is built.
    from_path(&paths.env_path).ok();

    // The .env value is intentionally authoritative for logging, even when
    // the shell inherited RUST_LOG. Read it as configuration instead of
    // mutating the process environment after runtime creation.
    let logging = LoggingSettings::load(&paths.env_path);

    #[allow(clippy::print_stderr)]
    let log_writer = match logging::build_writer(&paths.log_dir, logging.log_level) {
        Ok(writer) => writer,
        Err(error) => {
            eprintln!("Failed to initialize logfile: {error:#}");
            std::process::exit(1);
        }
    };
    let terminal_layer = tracing_subscriber::fmt::layer();
    let file_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(log_writer.writer());

    #[allow(clippy::print_stderr)]
    if let Err(error) = tracing_subscriber::registry()
        .with(logging.filter)
        .with(terminal_layer)
        .with(file_layer)
        .try_init()
    {
        eprintln!("Failed to initialize logging: {error}");
        std::process::exit(1);
    }

    if let Some(error) = logging.invalid_filter {
        tracing::warn!(?error, "Invalid RUST_LOG filter; using the default filter");
    }

    let _log_guard = log_writer;

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

    if let Err(error) = runtime.block_on(run(startup.invocation, paths)) {
        tracing::error!("Chester failed to start: {error:#}");
        tracing::debug!(error = ?error, "Startup error chain");
        std::process::exit(1);
    }
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

#[derive(Debug)]
struct StartupOptions {
    runtime_root: std::path::PathBuf,
    config_path: Option<std::path::PathBuf>,
    invocation: Invocation,
}

#[derive(Debug, PartialEq, Eq)]
enum Invocation {
    Bot,
    Evaluation(EvaluationCommand),
}

#[derive(Debug, PartialEq, Eq)]
enum EvaluationCommand {
    Chronicle {
        suite_path: std::path::PathBuf,
        report_path: Option<std::path::PathBuf>,
    },
    Query {
        suite_path: std::path::PathBuf,
        report_path: Option<std::path::PathBuf>,
        test_planner: bool,
    },
    Synthesis {
        suite_path: std::path::PathBuf,
        report_path: Option<std::path::PathBuf>,
    },
}

impl StartupOptions {
    fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Self> {
        let mut runtime_root = None;
        let mut config_path = None;
        let mut command_arguments = Vec::new();
        let mut arguments = arguments.into_iter();
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--runtime-root" => {
                    runtime_root = Some(
                        arguments
                            .next()
                            .context("--runtime-root requires a directory")?
                            .into(),
                    );
                }
                "--config" => {
                    config_path = Some(
                        arguments
                            .next()
                            .context("--config requires a file path")?
                            .into(),
                    );
                }
                "--help" | "-h" => anyhow::bail!(
                    "Usage: chester-rs [--runtime-root DIR] [--config FILE] [evaluation command]"
                ),
                _ => command_arguments.push(argument),
            }
        }
        Ok(Self {
            runtime_root: runtime_root.unwrap_or(
                std::env::current_dir().context("Failed to determine current directory")?,
            ),
            config_path,
            invocation: Invocation::parse(command_arguments)?,
        })
    }
}

impl Invocation {
    fn parse(arguments: Vec<String>) -> Result<Self> {
        let Some((command, arguments)) = arguments.split_first() else {
            return Ok(Self::Bot);
        };
        match command.as_str() {
            "--chronicle-synthesis-eval" => {
                anyhow::ensure!(
                    arguments.len() <= 2,
                    "Usage: chester-rs --chronicle-synthesis-eval [SUITE.toml] [REPORT.json]"
                );
                Ok(Self::Evaluation(EvaluationCommand::Synthesis {
                    suite_path: arguments
                        .first()
                        .map_or_else(|| DEFAULT_CHRONICLE_SYNTHESIS_EVAL_SUITE.into(), Into::into),
                    report_path: arguments.get(1).map(Into::into),
                }))
            }
            "--chronicle-query-eval" => {
                let (arguments, test_planner) = match arguments.strip_suffix(&["--planner".into()])
                {
                    Some(arguments) => (arguments, true),
                    None => (arguments, false),
                };
                anyhow::ensure!(
                    arguments.len() <= 2,
                    "Usage: chester-rs --chronicle-query-eval [SUITE.toml] [REPORT.json] [--planner]"
                );
                Ok(Self::Evaluation(EvaluationCommand::Query {
                    suite_path: arguments
                        .first()
                        .map_or_else(|| DEFAULT_CHRONICLE_QUERY_EVAL_SUITE.into(), Into::into),
                    report_path: arguments.get(1).map(Into::into),
                    test_planner,
                }))
            }
            "--chronicle-eval" => {
                anyhow::ensure!(
                    arguments.len() <= 2,
                    "Usage: chester-rs --chronicle-eval [SUITE.toml] [REPORT.json]"
                );
                Ok(Self::Evaluation(EvaluationCommand::Chronicle {
                    suite_path: arguments
                        .first()
                        .map_or_else(|| DEFAULT_CHRONICLE_EVAL_SUITE.into(), Into::into),
                    report_path: arguments.get(1).map(Into::into),
                }))
            }
            _ => Ok(Self::Bot),
        }
    }
}

async fn run(invocation: Invocation, paths: AppPaths) -> Result<()> {
    match invocation {
        Invocation::Bot => run_bot(paths).await,
        Invocation::Evaluation(EvaluationCommand::Synthesis {
            suite_path,
            report_path,
        }) => {
            chronicle::synthesis_eval::runner::run(&suite_path, report_path.as_deref(), &paths)
                .await
        }
        Invocation::Evaluation(EvaluationCommand::Query {
            suite_path,
            report_path,
            test_planner,
        }) => {
            chronicle::query::eval::run(&suite_path, report_path.as_deref(), test_planner, &paths)
                .await
        }
        Invocation::Evaluation(EvaluationCommand::Chronicle {
            suite_path,
            report_path,
        }) => chronicle::eval::run(&suite_path, report_path.as_deref(), &paths).await,
    }
}

async fn run_bot(paths: AppPaths) -> Result<()> {
    tracing::info!(runtime_root = %paths.runtime_root.display(), config_path = %paths.config_path.display(), "Starting Chester");

    let config_path = paths.config_path.clone();
    let config = Config::load(paths).with_context(|| {
        format!(
            "Failed to load configuration from {}",
            config_path.display()
        )
    })?;
    tracing::debug!(
        corpus_dir = %config.chronicle.indexing.corpus_dir.display(),
        retrieval_limit = config.chronicle.retrieval.limit,
        max_chunk_tokens = config.chronicle.indexing.max_chunk_tokens,
        max_reply_length = config.chronicle.llm.generation.max_reply_length,
        "Loaded configuration"
    );

    scan_incomplete_manifests(&config.paths.recordings_dir)
        .context("Failed to scan recording manifests")?;

    let token = std::env::var("DISCORD_TOKEN").context("DISCORD_TOKEN is not set")?;

    let pool = database::pool::open_sqlite_pool(&config.database.jester, "Jester")
        .await
        .context("Failed to open the Jester database")?;
    jester::db::schema::initialise(&pool)
        .await
        .context("Failed to initialize the Jester database schema")?;
    let chronicle = Arc::new(
        build_chronicle(&config)
            .await
            .context("Failed to initialize Chronicle")?,
    );

    let downloader = Downloader::new(crate::jester::track::download::DownloadConfig::from(
        &config.paths,
    ));
    let sync_stats = sync_audio_library(
        &pool,
        SyncConfig {
            downloader: downloader.clone(),
        },
    )
    .await
    .context("Failed to synchronize the audio library")?;

    info!(
        downloaded = sync_stats.downloaded,
        failed = sync_stats.failed,
        skipped = sync_stats.skipped,
        "Library sync complete"
    );

    if sync_stats.failed > 0 {
        bail!(
            "Audio library synchronization failed for {} track(s); refusing to start",
            sync_stats.failed
        );
    }

    let poise_commands = build_commands();
    tracing::info!(
        command_count = poise_commands.len(),
        "Registering bot commands"
    );

    // Build the Songbird config too (required for decoding voice data).
    let songbird_config =
        SongbirdConfig::default().decode_mode(DecodeMode::Decode(DecodeConfig::default()));

    let songbird = songbird::Songbird::serenity_from_config(songbird_config.clone());
    let recorder = crate::chronicle::recording::recorder::RecorderManager::new(
        config.paths.recordings_dir.clone(),
    );
    let player = Arc::new(jester::player::service::PlayerService::new(
        config.paths.audio_dir.clone(),
    ));
    let coordinator = Arc::new(shutdown::ShutdownCoordinator::new(
        recorder.clone(),
        player.clone(),
        chronicle.clone(),
        pool.clone(),
        songbird.clone(),
        Duration::from_secs(20),
    ));

    let framework = build_framework(
        pool,
        config,
        chronicle,
        recorder,
        player,
        coordinator.state.clone(),
        downloader,
        poise_commands,
    );

    let intents = GatewayIntents::non_privileged() | GatewayIntents::MESSAGE_CONTENT;

    // 3) Create the Serenity client, attach Poise as the event handler…
    // 4) And register Songbird on the same builder
    let mut client = ClientBuilder::new(token, intents)
        .framework(framework)
        .register_songbird_with(songbird) // ← this injects the Songbird voice manager
        .await
        .context("Failed to create the Discord client")?;

    tracing::info!("Starting Discord gateway");
    let signal = shutdown_signal();
    tokio::pin!(signal);
    tokio::select! {
        result = client.start() => {
            let gateway_result = result.context("Discord gateway stopped with an error");
            let drain_result = coordinator.drain().await;
            gateway_result.and(drain_result)?;
        }
        _ = &mut signal => {
            tracing::info!("Shutdown signal received");
            coordinator.drain().await?;
        }
    }

    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod startup_tests {
    use super::{
        EvaluationCommand, Invocation, StartupOptions, configured_log_level, selected_log_directive,
    };
    use std::path::PathBuf;

    #[test]
    fn separates_runtime_options_from_evaluation_arguments() -> anyhow::Result<()> {
        let options = StartupOptions::parse([
            "--runtime-root".into(),
            "/srv/chester".into(),
            "--config".into(),
            "config/production.toml".into(),
            "--chronicle-eval".into(),
            "suite.toml".into(),
        ])?;

        assert_eq!(options.runtime_root, PathBuf::from("/srv/chester"));
        assert_eq!(
            options.config_path,
            Some(PathBuf::from("config/production.toml"))
        );
        assert_eq!(
            options.invocation,
            Invocation::Evaluation(EvaluationCommand::Chronicle {
                suite_path: PathBuf::from("suite.toml"),
                report_path: None,
            })
        );
        Ok(())
    }

    #[test]
    fn rejects_runtime_options_without_values() {
        assert!(StartupOptions::parse(["--config".into()]).is_err());
        assert!(StartupOptions::parse(["--runtime-root".into()]).is_err());
    }

    #[test]
    fn parses_query_evaluation_options() -> anyhow::Result<()> {
        let options = StartupOptions::parse([
            "--chronicle-query-eval".into(),
            "suite.toml".into(),
            "report.json".into(),
            "--planner".into(),
        ])?;

        assert_eq!(
            options.invocation,
            Invocation::Evaluation(EvaluationCommand::Query {
                suite_path: PathBuf::from("suite.toml"),
                report_path: Some(PathBuf::from("report.json")),
                test_planner: true,
            })
        );
        Ok(())
    }

    #[test]
    fn rejects_too_many_evaluation_arguments() {
        assert!(
            StartupOptions::parse([
                "--chronicle-eval".into(),
                "suite.toml".into(),
                "report.json".into(),
                "extra".into(),
            ])
            .is_err()
        );
    }

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
