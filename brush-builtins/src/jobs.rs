use std::io::Write;

use brush_core::{ExecutionResult, builtins, error, jobs};

/// Manage jobs.
#[derive(winnow_args::Args)]
#[arg(disable_help_short, disable_version_flag, disable_help_subcommand)]
pub(crate) struct JobsCommand {
    /// Also show process IDs.
    #[arg(short = 'l')]
    also_show_pids: bool,

    /// List only jobs that have changed status since the last notification.
    #[arg(short = 'n')]
    list_changed_only: bool,

    /// Show only process IDs.
    #[arg(short = 'p')]
    show_pids_only: bool,

    /// Show only running jobs.
    #[arg(short = 'r')]
    running_jobs_only: bool,

    /// Show only stopped jobs.
    #[arg(short = 's')]
    stopped_jobs_only: bool,

    /// Job specs to list.
    // TODO(jobs): Add -x option
    #[arg(positional)]
    job_specs: Vec<String>,
}

brush_winnow_args_adapter::impl_parse_args!(JobsCommand);

impl<SE: brush_core::ShellExtensions> builtins::Builtin<SE> for JobsCommand {
    type Error = brush_core::Error;

    async fn execute(
        self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<brush_core::ExecutionResult, Self::Error> {
        if self.list_changed_only {
            return error::unimp("jobs -n");
        }

        // Notice the jobs that have finished, so they're listed as done.
        for job in &mut context.shell.jobs_mut().jobs {
            let _ = job.poll_done()?;
        }

        let mut result = ExecutionResult::success();
        if self.job_specs.is_empty() {
            for job in &context.shell.jobs().jobs {
                self.display_job(&context, job)?;
            }
        } else {
            for spec in &self.job_specs {
                let id = context
                    .shell
                    .jobs_mut()
                    .resolve_job_spec(spec)
                    .map(|j| j.id);
                if let Some(job) =
                    id.and_then(|id| context.shell.jobs().jobs.iter().find(|j| j.id == id))
                {
                    self.display_job(&context, job)?;
                } else {
                    writeln!(
                        context.stderr(),
                        "{}: {spec}: no such job",
                        context.command_name
                    )?;
                    result = ExecutionResult::general_error();
                }
            }
        }

        Ok(result)
    }
}

impl JobsCommand {
    fn display_job(
        &self,
        context: &brush_core::ExecutionContext<'_, impl brush_core::ShellExtensions>,
        job: &jobs::Job,
    ) -> Result<(), brush_core::Error> {
        if self.running_jobs_only && !matches!(job.state, jobs::JobState::Running) {
            return Ok(());
        }
        if self.stopped_jobs_only && !matches!(job.state, jobs::JobState::Stopped) {
            return Ok(());
        }

        if self.show_pids_only {
            if let Some(pid) = job.representative_pid() {
                writeln!(context.stdout(), "{pid}")?;
            }
        } else if self.also_show_pids {
            let pid = job
                .representative_pid()
                .map_or_else(String::new, |pid| pid.to_string());
            writeln!(
                context.stdout(),
                "[{}]{:<2}{pid} {:<24}{}",
                job.id,
                job.annotation(),
                job.state,
                job.command_line
            )?;
        } else {
            writeln!(context.stdout(), "{job}")?;
        }

        Ok(())
    }
}
