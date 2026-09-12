use anyhow::Result;

use crate::chronicle::{AppPaths, run_eval, run_planner, run_query, run_synthesis_eval};

use super::cli::EvaluationCommand;

pub async fn run(command: EvaluationCommand, paths: AppPaths) -> Result<()> {
    match command {
        EvaluationCommand::Synthesis {
            suite_path,
            report_path,
        } => run_synthesis_eval(&suite_path, report_path.as_deref(), &paths).await,
        EvaluationCommand::Query {
            suite_path,
            report_path,
        } => run_query(&suite_path, report_path.as_deref(), &paths).await,
        EvaluationCommand::QueryPlanner {
            suite_path,
            report_path,
        } => run_planner(&suite_path, report_path.as_deref(), &paths).await,
        EvaluationCommand::Chronicle {
            suite_path,
            report_path,
        } => run_eval(&suite_path, report_path.as_deref(), &paths).await,
    }
}
