use anyhow::{Context, Result};
use std::path::PathBuf;

const DEFAULT_CHRONICLE_EVAL_SUITE: &str = "tests/fixtures/chronicle/suite.toml";
const DEFAULT_CHRONICLE_QUERY_EVAL_SUITE: &str = "tests/fixtures/chronicle-query/suite.toml";
const DEFAULT_CHRONICLE_SYNTHESIS_EVAL_SUITE: &str =
    "tests/fixtures/chronicle-synthesis/suite.toml";

#[derive(Debug)]
pub struct StartupOptions {
    pub runtime_root: PathBuf,
    pub config_path: Option<PathBuf>,
    pub invocation: Invocation,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Invocation {
    Bot,
    Evaluation(EvaluationCommand),
}

#[derive(Debug, PartialEq, Eq)]
pub enum EvaluationCommand {
    Chronicle {
        suite_path: PathBuf,
        report_path: Option<PathBuf>,
    },
    Query {
        suite_path: PathBuf,
        report_path: Option<PathBuf>,
    },
    QueryPlanner {
        suite_path: PathBuf,
        report_path: Option<PathBuf>,
    },
    Synthesis {
        suite_path: PathBuf,
        report_path: Option<PathBuf>,
    },
}

impl StartupOptions {
    pub fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Self> {
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
            invocation: Invocation::parse(&command_arguments)?,
        })
    }
}

impl Invocation {
    fn parse(arguments: &[String]) -> Result<Self> {
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
                anyhow::ensure!(
                    !arguments.iter().any(|argument| argument == "--planner"),
                    "The planner evaluation is now invoked with --chronicle-query-planner-eval"
                );
                anyhow::ensure!(
                    arguments.len() <= 2,
                    "Usage: chester-rs --chronicle-query-eval [SUITE.toml] [REPORT.json]"
                );
                Ok(Self::Evaluation(EvaluationCommand::Query {
                    suite_path: arguments
                        .first()
                        .map_or_else(|| DEFAULT_CHRONICLE_QUERY_EVAL_SUITE.into(), Into::into),
                    report_path: arguments.get(1).map(Into::into),
                }))
            }
            "--chronicle-query-planner-eval" => {
                anyhow::ensure!(
                    arguments.len() <= 2,
                    "Usage: chester-rs --chronicle-query-planner-eval [SUITE.toml] [REPORT.json]"
                );
                Ok(Self::Evaluation(EvaluationCommand::QueryPlanner {
                    suite_path: arguments
                        .first()
                        .map_or_else(|| DEFAULT_CHRONICLE_QUERY_EVAL_SUITE.into(), Into::into),
                    report_path: arguments.get(1).map(Into::into),
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

#[cfg(test)]
mod tests {
    use super::*;

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
        ])?;
        assert_eq!(
            options.invocation,
            Invocation::Evaluation(EvaluationCommand::Query {
                suite_path: PathBuf::from("suite.toml"),
                report_path: Some(PathBuf::from("report.json")),
            })
        );
        Ok(())
    }

    #[test]
    fn parses_query_planner_evaluation_options() -> anyhow::Result<()> {
        let options = StartupOptions::parse([
            "--chronicle-query-planner-eval".into(),
            "suite.toml".into(),
            "report.json".into(),
        ])?;
        assert_eq!(
            options.invocation,
            Invocation::Evaluation(EvaluationCommand::QueryPlanner {
                suite_path: PathBuf::from("suite.toml"),
                report_path: Some(PathBuf::from("report.json")),
            })
        );
        Ok(())
    }

    #[test]
    fn rejects_nested_query_planner_option() {
        assert!(
            StartupOptions::parse(["--chronicle-query-eval".into(), "--planner".into()]).is_err()
        );
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
}
