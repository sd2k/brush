//! Job management

use std::borrow::Cow;
use std::collections::VecDeque;
use std::fmt::Display;

use futures::FutureExt;

use crate::ExecutionExitCode;
use crate::ExecutionResult;
use crate::error;
use crate::processes;
use crate::sys;
use crate::tasks;
use crate::trace_categories;
use crate::traps;

pub(crate) type JobResult = (Job, Result<ExecutionResult, error::Error>);

/// The first ID [`JobManager`] gives a job of its own: one more than the largest
/// process ID Linux hands out, so that no process has one.
const FIRST_JOB_PID: sys::process::ProcessId = (1 << 22) + 1;

/// Manages the jobs that are currently managed by the shell.
#[derive(Default)]
pub struct JobManager {
    /// The jobs that are currently managed by the shell.
    pub jobs: Vec<Job>,

    /// The ID of the last job given one of the shell's own.
    last_job_pid: Option<sys::process::ProcessId>,
}

/// Represents a task that is part of a job.
pub enum JobTask {
    /// An external process.
    External(processes::ChildProcess),
    /// An internal asynchronous task.
    Internal(tasks::TaskHandle<Result<ExecutionResult, error::Error>>),
}

/// Represents the result of waiting on a job task.
pub enum JobTaskWaitResult {
    /// The task has completed.
    Completed(ExecutionResult),
    /// The task was stopped.
    Stopped,
}

impl JobTask {
    /// Returns whether the task is an external process.
    pub const fn is_external(&self) -> bool {
        matches!(self, Self::External(_))
    }

    /// Waits for the task to complete. Returns the result of the wait.
    pub async fn wait(&mut self) -> Result<JobTaskWaitResult, error::Error> {
        match self {
            Self::External(process) => {
                let wait_result = process.wait().await?;
                match wait_result {
                    processes::ProcessWaitResult::Completed(result) => {
                        Ok(JobTaskWaitResult::Completed(result))
                    }
                    processes::ProcessWaitResult::Stopped => Ok(JobTaskWaitResult::Stopped),
                }
            }
            Self::Internal(handle) => Ok(JobTaskWaitResult::Completed(handle.await??)),
        }
    }

    /// Polls the task for completion. Returns `Some(result)` if the task has completed,
    /// or `None` if it is still running. The result is the execution result of the task.
    /// Behaves in a best-effort manner; if an internal error occurs during polling,
    /// it will return `None`.
    fn poll(&mut self) -> Option<Result<ExecutionResult, error::Error>> {
        match self {
            Self::External(process) => process.poll(),
            Self::Internal(handle) => {
                let checkable_handle = handle;
                checkable_handle.now_or_never().and_then(|r| r.ok())
            }
        }
    }
}

impl JobManager {
    /// Returns a new job manager.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a job to the job manager and marks it as the current job;
    /// returns an immutable reference to the job.
    ///
    /// # Arguments
    ///
    /// * `job` - The job to add.
    #[allow(
        clippy::missing_panics_doc,
        reason = "push() guarantees the vector length is >= 1"
    )]
    pub fn add_as_current(&mut self, mut job: Job) -> &Job {
        for j in &mut self.jobs {
            if matches!(j.annotation, JobAnnotation::Current) {
                j.annotation = JobAnnotation::Previous;
                break;
            }
        }

        let id = self.jobs.len() + 1;
        job.id = id;
        job.annotation = JobAnnotation::Current;
        // A job with no process of its own (e.g., one running in a task of the shell's) still
        // needs an ID for `$!`, `wait`, and `kill`.
        let pid = self
            .last_job_pid
            .map_or(FIRST_JOB_PID, |pid| pid.wrapping_add(1).max(FIRST_JOB_PID));
        self.last_job_pid = Some(pid);
        job.own_pid = Some(pid);
        self.jobs.push(job);

        #[allow(clippy::unwrap_used, reason = "we just pushed an element")]
        self.jobs.last().unwrap()
    }

    /// Returns the current job, if there is one.
    pub fn current_job(&self) -> Option<&Job> {
        self.jobs
            .iter()
            .find(|j| matches!(j.annotation, JobAnnotation::Current))
    }

    /// Returns a mutable reference to the current job, if there is one.
    pub fn current_job_mut(&mut self) -> Option<&mut Job> {
        self.jobs
            .iter_mut()
            .find(|j| matches!(j.annotation, JobAnnotation::Current))
    }

    /// Returns the previous job, if there is one.
    pub fn prev_job(&self) -> Option<&Job> {
        self.jobs
            .iter()
            .find(|j| matches!(j.annotation, JobAnnotation::Previous))
    }

    /// Returns a mutable reference to the previous job, if there is one.
    pub fn prev_job_mut(&mut self) -> Option<&mut Job> {
        self.jobs
            .iter_mut()
            .find(|j| matches!(j.annotation, JobAnnotation::Previous))
    }

    /// Tries to resolve the given job specification to a job.
    ///
    /// # Arguments
    ///
    /// * `job_spec` - The job specification to resolve.
    pub fn resolve_job_spec(&mut self, job_spec: &str) -> Option<&mut Job> {
        let remainder = job_spec.strip_prefix('%')?;

        match remainder {
            "%" | "+" => self.current_job_mut(),
            "-" => self.prev_job_mut(),
            s if s.chars().all(char::is_numeric) => {
                let id = s.parse::<usize>().ok()?;
                self.jobs.iter_mut().find(|j| j.id == id)
            }
            _ => {
                tracing::warn!(target: trace_categories::UNIMPLEMENTED, "unimplemented: job spec naming command: '{job_spec}'");
                None
            }
        }
    }

    /// Returns the job with the given process ID: its representative one, or the ID the shell
    /// gave it.
    ///
    /// # Arguments
    ///
    /// * `pid` - The process ID to look for.
    pub fn find_by_pid(&mut self, pid: sys::process::ProcessId) -> Option<&mut Job> {
        self.jobs
            .iter_mut()
            .find(|j| j.representative_pid() == Some(pid))
    }

    /// Removes the job with the given ID, returning it.
    ///
    /// # Arguments
    ///
    /// * `id` - The shell-internal ID of the job.
    pub fn remove(&mut self, id: usize) -> Option<Job> {
        let i = self.jobs.iter().position(|j| j.id == id)?;
        Some(self.jobs.remove(i))
    }

    /// Waits for the first of the given jobs to complete, or of all of them if `ids` is
    /// `None`; returns its ID and result, or `None` if there's no such job to wait for.
    ///
    /// # Arguments
    ///
    /// * `ids` - The shell-internal IDs of the jobs to wait for.
    pub async fn wait_any(
        &mut self,
        ids: Option<&[usize]>,
    ) -> Option<(usize, Result<ExecutionResult, error::Error>)> {
        let waits: Vec<_> = self
            .jobs
            .iter_mut()
            .filter(|j| ids.is_none_or(|ids| ids.contains(&j.id)))
            .map(|j| {
                Box::pin(async move {
                    let id = j.id;
                    (id, j.wait().await)
                })
            })
            .collect();
        if waits.is_empty() {
            return None;
        }
        Some(futures::future::select_all(waits).await.0)
    }

    /// Waits for all managed jobs to complete.
    pub async fn wait_all(&mut self) -> Result<Vec<Job>, error::Error> {
        for job in &mut self.jobs {
            // The caller wants the jobs, not their statuses: `wait` with no operands returns 0
            // whatever they were.
            let _ = job.wait().await?;
        }

        Ok(self.sweep_completed_jobs())
    }

    /// Polls all managed jobs for completion.
    pub fn poll(&mut self) -> Result<Vec<JobResult>, error::Error> {
        let mut results = Vec::with_capacity(self.jobs.len());

        let mut i = 0;
        while i != self.jobs.len() {
            if let Some(result) = self.jobs[i].poll_done()? {
                let job = self.jobs.remove(i);
                results.push((job, result));
            } else if matches!(self.jobs[i].state, JobState::Done) {
                // TODO(jobs): This is a workaround to remove jobs that are done but for which we
                // don't know what happened.
                results.push((self.jobs.remove(i), Ok(ExecutionResult::success())));
            } else {
                i += 1;
            }
        }

        Ok(results)
    }

    fn sweep_completed_jobs(&mut self) -> Vec<Job> {
        let mut completed_jobs = vec![];

        let mut i = 0;
        while i != self.jobs.len() {
            if self.jobs[i].tasks.is_empty() {
                completed_jobs.push(self.jobs.remove(i));
            } else {
                i += 1;
            }
        }

        completed_jobs
    }
}

/// Represents the current execution state of a job.
#[derive(Clone)]
pub enum JobState {
    /// Unknown state.
    Unknown,
    /// The job is running.
    Running,
    /// The job is stopped.
    Stopped,
    /// The job has completed.
    Done,
}

impl Display for JobState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown => f.pad("Unknown"),
            Self::Running => f.pad("Running"),
            Self::Stopped => f.pad("Stopped"),
            Self::Done => f.pad("Done"),
        }
    }
}

/// Represents an annotation for a job.
#[derive(Clone)]
pub enum JobAnnotation {
    /// No annotation.
    None,
    /// The job is the current job.
    Current,
    /// The job is the previous job.
    Previous,
}

impl Display for JobAnnotation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => f.pad(""),
            Self::Current => f.pad("+"),
            Self::Previous => f.pad("-"),
        }
    }
}

/// Encapsulates a set of processes managed by the shell as a single unit.
pub struct Job {
    /// The tasks that make up the job.
    tasks: VecDeque<JobTask>,

    /// The annotation of the job (e.g., current, previous).
    annotation: JobAnnotation,

    /// The shell-internal ID of the job.
    pub id: usize,

    /// The command line of the job.
    pub command_line: String,

    /// The current operational state of the job.
    pub state: JobState,

    /// The ID the shell gave the job, for when none of its processes has one.
    own_pid: Option<sys::process::ProcessId>,

    /// The job's status, once it's known to have finished.
    status: Option<ExecutionExitCode>,
}

impl Display for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "[{}]{:3}{}\t{}",
            self.id, self.annotation, self.state, self.command_line
        )
    }
}

impl Job {
    /// Returns a new job object.
    ///
    /// # Arguments
    ///
    /// * `children` - The job's known child processes.
    /// * `command_line` - The command line of the job.
    /// * `state` - The current operational state of the job.
    pub(crate) fn new<I>(tasks: I, command_line: String, state: JobState) -> Self
    where
        I: IntoIterator<Item = JobTask>,
    {
        Self {
            id: 0,
            tasks: tasks.into_iter().collect(),
            annotation: JobAnnotation::None,
            command_line,
            state,
            own_pid: None,
            status: None,
        }
    }

    /// Returns a pid-style string for the job.
    pub fn to_pid_style_string(&self) -> String {
        let display_pid = self
            .representative_pid()
            .map_or(Cow::Borrowed("<pid unknown>"), |pid| {
                Cow::Owned(pid.to_string())
            });
        std::format!("[{}]{}\t{}", self.id, self.annotation, display_pid)
    }

    /// Returns the annotation of the job.
    pub fn annotation(&self) -> JobAnnotation {
        self.annotation.clone()
    }

    /// Returns the command name of the job.
    pub fn command_name(&self) -> &str {
        self.command_line
            .split_ascii_whitespace()
            .next()
            .unwrap_or_default()
    }

    /// Returns whether the job is the current job.
    pub const fn is_current(&self) -> bool {
        matches!(self.annotation, JobAnnotation::Current)
    }

    /// Returns whether the job is the previous job.
    pub const fn is_prev(&self) -> bool {
        matches!(self.annotation, JobAnnotation::Previous)
    }

    /// Polls whether the job has completed.
    pub fn poll_done(
        &mut self,
    ) -> Result<Option<Result<ExecutionResult, error::Error>>, error::Error> {
        let mut result: Option<Result<ExecutionResult, error::Error>> =
            self.status.map(|status| Ok(status.into()));

        tracing::debug!(target: trace_categories::JOBS, "Polling job {} for completion...", self.id);

        while !self.tasks.is_empty() {
            let task = &mut self.tasks[0];
            match task.poll() {
                Some(r) => {
                    self.tasks.remove(0);
                    result = Some(r);
                }
                None => {
                    return Ok(None);
                }
            }
        }

        tracing::debug!(target: trace_categories::JOBS, "Job {} has completed.", self.id);

        self.state = JobState::Done;
        // Keep its status for a later `wait`, which finds no tasks left to wait for.
        if let Some(Ok(done)) = &result {
            self.status = Some(done.exit_code);
        }

        Ok(result)
    }

    /// Waits for the job to complete.
    pub async fn wait(&mut self) -> Result<ExecutionResult, error::Error> {
        let mut result = self
            .status
            .map_or_else(ExecutionResult::success, ExecutionResult::from);

        while let Some(task) = self.tasks.back_mut() {
            match task.wait().await? {
                JobTaskWaitResult::Completed(execution_result) => {
                    result = execution_result;
                    self.tasks.pop_back();
                }
                JobTaskWaitResult::Stopped => {
                    self.state = JobState::Stopped;
                    return Ok(ExecutionResult::stopped());
                }
            }
        }

        self.state = JobState::Done;

        Ok(result)
    }

    /// Moves the job to execute in the background.
    pub fn move_to_background(&mut self) -> Result<(), error::Error> {
        self.continue_job()
    }

    /// Moves the job to execute in the foreground.
    pub fn move_to_foreground(&mut self) -> Result<(), error::Error> {
        // The job gets the terminal before it's continued, so that it doesn't find itself in the
        // background and stop again on reading from the terminal. (Without a terminal, as in a
        // script that enables job control, there's none to give it, and it's continued anyway.)
        if let Some(pgid) = self.own_process_group()
            && let Err(err) = sys::terminal::move_to_foreground(pgid)
        {
            tracing::debug!(target: trace_categories::JOBS, "not giving job {} the terminal: {err}", self.id);
        }

        self.continue_job()
    }

    /// Continues the job, and records it running.
    fn continue_job(&mut self) -> Result<(), error::Error> {
        if matches!(self.state, JobState::Stopped) {
            self.continue_processes()?;
            self.state = JobState::Running;
        } else {
            // As bash does for `fg`, continue it anyway: it may have been stopped without the
            // shell noticing yet (e.g., by reading from the terminal in the background).
            let _ = self.continue_processes();
        }

        Ok(())
    }

    /// Continues the job's stopped processes: all of its process group, if it has one.
    fn continue_processes(&self) -> Result<(), error::Error> {
        if let Some(pgid) = self.own_process_group() {
            sys::signal::continue_process_group(pgid)
        } else if let Some(pid) = self.process_pid() {
            sys::signal::continue_process(pid)
        } else {
            Err(error::ErrorKind::FailedToSendSignal.into())
        }
    }

    /// Checks whether the job can be signaled.
    pub fn check_signalable(&self) -> Result<(), error::Error> {
        if let Some(pgid) = self.own_process_group() {
            sys::signal::check_process_group_signalable(pgid)
        } else if let Some(pid) = self.process_pid() {
            sys::signal::check_signalable(pid)
        } else if !self.tasks.is_empty() {
            // It runs in the shell's own tasks, which `kill` can abort.
            Ok(())
        } else {
            Err(error::ErrorKind::FailedToSendSignal.into())
        }
    }

    /// Kills the job: sends the signal to all of its process group, if it has one. A job sent
    /// SIGTERM or SIGHUP is also continued, so that it acts on it if it's stopped.
    ///
    /// # Arguments
    ///
    /// * `signal` - The signal to send to the job.
    pub fn kill(&mut self, signal: traps::TrapSignal) -> Result<(), error::Error> {
        if let Some(pgid) = self.own_process_group() {
            sys::signal::kill_process_group(pgid, signal)?;
        } else if let Some(pid) = self.process_pid() {
            sys::signal::kill_process(pid, signal)?;
        } else {
            // A job running in the shell's own tasks has no process to signal; a host without
            // signals can `abort` it instead.
            return Err(error::ErrorKind::FailedToSendSignal.into());
        }

        // As bash does, continue a stopped job sent SIGTERM or SIGHUP, so that it acts on it;
        // and one not known to be stopped, which may have stopped without the shell's noticing.
        if sys::signal::continue_stopped_on(signal) {
            self.continue_job()?;
        }

        Ok(())
    }

    /// Stops the job without signaling it, by aborting its tasks, and records it done with the
    /// given status. Returns false, and does nothing, if any of its tasks is a process, or
    /// can't be aborted.
    ///
    /// # Arguments
    ///
    /// * `status` - The status the job is to have, as if it had exited with it.
    pub fn abort(&mut self, status: ExecutionExitCode) -> bool {
        if self.tasks.iter().any(JobTask::is_external) {
            return false;
        }
        for task in &self.tasks {
            if let JobTask::Internal(handle) = task
                && !handle.abort()
            {
                return false;
            }
        }
        self.tasks.clear();
        self.state = JobState::Done;
        self.status = Some(status);
        true
    }

    /// Tries to retrieve a "representative" pid for the job: that of its first process with
    /// one, or else the ID the shell gave it.
    pub fn representative_pid(&self) -> Option<sys::process::ProcessId> {
        self.process_pid().or(self.own_pid)
    }

    /// Returns the process ID of the job's first process with one.
    fn process_pid(&self) -> Option<sys::process::ProcessId> {
        for task in &self.tasks {
            match task {
                JobTask::External(p) => {
                    if let Some(pid) = p.pid() {
                        return Some(pid);
                    }
                }
                JobTask::Internal(_) => (),
            }
        }
        None
    }

    /// Tries to retrieve the process group ID (PGID) of the job: that of its own process group,
    /// if it has one, or else its representative pid.
    pub fn process_group_id(&self) -> Option<sys::process::ProcessId> {
        self.own_process_group().or_else(|| self.process_pid())
    }

    /// Returns the job's own process group, if it has one: under job control, its processes
    /// run in a group of their own, which needn't be led by the first of them.
    fn own_process_group(&self) -> Option<sys::process::ProcessId> {
        self.tasks.iter().find_map(|task| match task {
            JobTask::External(child) => child.pgid(),
            JobTask::Internal(_) => None,
        })
    }
}

#[cfg(test)]
#[allow(clippy::panic_in_result_fn, reason = "assertions in a fallible test")]
mod tests {
    use super::*;

    /// A job running a task that never finishes, through a handle that can't abort it.
    fn pending_job() -> Job {
        let task =
            sys::tasks::spawn(std::future::pending::<Result<ExecutionResult, error::Error>>());
        Job::new(
            [JobTask::Internal(tasks::TaskHandle::from_future(task))],
            "pending".into(),
            JobState::Running,
        )
    }

    #[tokio::test]
    async fn jobs_without_processes_get_distinct_pids() {
        let mut jobs = JobManager::new();
        let first = jobs.add_as_current(pending_job()).representative_pid();
        let second = jobs.add_as_current(pending_job()).representative_pid();
        assert!(first.is_some_and(|pid| pid >= FIRST_JOB_PID));
        assert_ne!(first, second);
        let second = second.unwrap_or_default();
        assert_eq!(jobs.find_by_pid(second).map(|j| j.id), Some(2));
    }

    #[tokio::test]
    async fn an_aborted_job_waits_with_its_status() -> Result<(), error::Error> {
        let mut jobs = JobManager::new();
        let task =
            sys::tasks::spawn(std::future::pending::<Result<ExecutionResult, error::Error>>());
        jobs.add_as_current(Job::new(
            [JobTask::Internal(task)],
            "pending".into(),
            JobState::Running,
        ));
        let job = jobs
            .current_job_mut()
            .ok_or(error::ErrorKind::FailedToSendSignal)?;
        assert!(job.abort(ExecutionExitCode::from(143)));
        assert!(matches!(job.state, JobState::Done));
        assert_eq!(u8::from(job.wait().await?.exit_code), 143);
        Ok(())
    }

    #[tokio::test]
    async fn a_job_whose_task_cannot_abort_is_left_alone() {
        let mut job = pending_job();
        assert!(!job.abort(ExecutionExitCode::from(143)));
        assert!(matches!(job.state, JobState::Running));
    }

    #[tokio::test]
    async fn wait_any_yields_the_first_job_to_finish() {
        let mut jobs = JobManager::new();
        jobs.add_as_current(pending_job());
        let done = sys::tasks::spawn(async { Ok(ExecutionResult::new(7)) });
        jobs.add_as_current(Job::new(
            [JobTask::Internal(done)],
            "done".into(),
            JobState::Running,
        ));
        let (id, result) = jobs
            .wait_any(None)
            .await
            .unwrap_or((0, Ok(ExecutionResult::success())));
        assert_eq!(id, 2);
        assert!(result.is_ok_and(|r| u8::from(r.exit_code) == 7));
        assert!(jobs.wait_any(Some(&[3])).await.is_none());
    }
}
