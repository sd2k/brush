use std::io::Write;

use brush_core::{ExecutionExitCode, ExecutionResult, builtins, env, error, variables};

/// Wait for jobs to terminate.
#[derive(winnow_args::Args)]
#[arg(disable_help_short, disable_version_flag, disable_help_subcommand)]
pub(crate) struct WaitCommand {
    /// Wait for specified job to terminate (instead of change status).
    #[arg(short = 'f')]
    wait_for_terminate: bool,

    /// Wait for a single job to change status; if jobs are specified, waits for
    /// the first to change status, and otherwise waits for the next change.
    #[arg(short = 'n')]
    wait_for_first_or_next: bool,

    /// Name of variable to receive the job ID of the job whose status is indicated.
    #[arg(short = 'p', value_name = "VAR_NAME")]
    variable_to_receive_id: Option<String>,

    /// Process IDs or job specs to wait for.
    #[arg(positional)]
    ids: Vec<String>,
}

brush_winnow_args_adapter::impl_parse_args!(WaitCommand);

impl<SE: brush_core::ShellExtensions> builtins::Builtin<SE> for WaitCommand {
    type Error = brush_core::Error;

    async fn execute(
        self,
        mut context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<ExecutionResult, Self::Error> {
        if self.wait_for_terminate {
            return error::unimp("wait -f");
        }

        // The jobs the operands name, by their shell-internal IDs. As bash does, an operand
        // that names no job of this shell's is reported and gives status 127 (or 1, if it's
        // neither a job spec nor a process ID).
        let mut job_ids = Vec::with_capacity(self.ids.len());
        let mut result = ExecutionResult::success();
        for id in &self.ids {
            let job_id = resolve(&mut context, id);
            if job_id.is_err() {
                if id.starts_with('%') {
                    writeln!(
                        context.stderr(),
                        "{}: {id}: no such job",
                        context.command_name
                    )?;
                } else if id.parse::<i32>().is_err() {
                    writeln!(
                        context.stderr(),
                        "{}: `{id}': not a pid or valid job spec",
                        context.command_name
                    )?;
                } else {
                    writeln!(
                        context.stderr(),
                        "{}: pid {id} is not a child of this shell",
                        context.command_name
                    )?;
                }
            }
            job_ids.push(job_id);
        }

        if self.wait_for_first_or_next {
            // The first of the named jobs (or of all jobs) to finish.
            let known: Vec<usize> = job_ids.iter().filter_map(|id| id.ok()).collect();
            let ids = (!self.ids.is_empty()).then_some(known.as_slice());
            let Some((job_id, job_result)) = context.shell.jobs_mut().wait_any(ids).await else {
                return Ok(ExecutionExitCode::NotFound.into());
            };
            return self.reap(&mut context, job_id, job_result);
        }

        if self.ids.is_empty() {
            // Wait for all jobs.
            let jobs = context.shell.jobs_mut().wait_all().await?;

            if context.shell.options().enable_job_control {
                for job in jobs {
                    writeln!(context.stdout(), "{job}")?;
                }
            }
            return Ok(result);
        }

        for job_id in job_ids {
            let job_id = match job_id {
                Ok(job_id) => job_id,
                Err(status) => {
                    result = status.into();
                    continue;
                }
            };
            let job_result = match context
                .shell
                .jobs_mut()
                .jobs
                .iter_mut()
                .find(|j| j.id == job_id)
            {
                Some(job) => job.wait().await,
                // Named twice, and already reaped.
                None => Ok(ExecutionExitCode::NotFound.into()),
            };
            result = self.reap(&mut context, job_id, job_result)?;
        }

        Ok(result)
    }
}

impl WaitCommand {
    /// Forgets a job that finished, as bash does once `wait` reports it, and returns its status,
    /// setting the `-p` variable to its ID.
    fn reap<SE: brush_core::ShellExtensions>(
        &self,
        context: &mut brush_core::ExecutionContext<'_, SE>,
        job_id: usize,
        job_result: Result<ExecutionResult, brush_core::Error>,
    ) -> Result<ExecutionResult, brush_core::Error> {
        // Take the job's status, but not its control flow: an `exit` in a background job ends
        // only that job.
        let status: ExecutionResult = job_result?.exit_code.into();
        if let Some(job) = context.shell.jobs_mut().remove(job_id)
            && let Some(var) = &self.variable_to_receive_id
        {
            let pid = job
                .representative_pid()
                .map_or_else(String::new, |pid| pid.to_string());
            context.shell.env_mut().update_or_add(
                var,
                variables::ShellValueLiteral::Scalar(pid),
                |_| Ok(()),
                env::EnvironmentLookup::Anywhere,
                env::EnvironmentScope::Global,
            )?;
        }
        Ok(status)
    }
}

/// Returns the shell-internal ID of the job that `id` names: a job spec, or a process ID; or
/// the status `wait` gives if it names none: 127, or 1 for an operand that's neither.
fn resolve<SE: brush_core::ShellExtensions>(
    context: &mut brush_core::ExecutionContext<'_, SE>,
    id: &str,
) -> Result<usize, ExecutionExitCode> {
    let jobs = context.shell.jobs_mut();
    let job = if id.starts_with('%') {
        jobs.resolve_job_spec(id)
    } else {
        let pid = id.parse().map_err(|_| ExecutionExitCode::GeneralError)?;
        jobs.find_by_pid(pid)
    };
    job.map(|j| j.id).ok_or(ExecutionExitCode::NotFound)
}
