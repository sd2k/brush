//! Process management utilities

use std::cell::RefCell;

pub(crate) type ProcessId = i32;

/// Provides access to a child process.
pub struct Child {
    inner: std::process::Child,
}

pub(crate) use std::process::ExitStatus;
pub(crate) use std::process::Output;

impl Child {
    /// Returns the process ID of the child process, if available.
    pub fn id(&self) -> Option<u32> {
        None
    }

    /// Asynchronously waits for the child process to exit.
    pub async fn wait(&mut self) -> std::io::Result<ExitStatus> {
        self.inner.wait()
    }

    /// Asynchronously waits for the child process to exit and collects its
    /// output.
    pub async fn wait_with_output(self) -> std::io::Result<Output> {
        self.inner.wait_with_output()
    }
}

pub(crate) fn spawn(mut command: std::process::Command) -> std::io::Result<Child> {
    let child = command.spawn()?;
    Ok(Child { inner: child })
}

// ============================================================================
// External command spawn handler for WASI environments
// ============================================================================

/// Errors from spawning an external command.
#[derive(Debug)]
pub enum SpawnError {
    /// The command was not found in the registry.
    NotFound(String),
    /// Spawning failed for some other reason.
    Failed(String),
}

impl std::fmt::Display for SpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(cmd) => write!(f, "command not found: {cmd}"),
            Self::Failed(msg) => write!(f, "spawn failed: {msg}"),
        }
    }
}

/// Trait for a child process spawned via the external handler.
///
/// Implementations bridge to the host's subprocess spawning mechanism
/// (e.g., WIT process interface).
pub trait ExternalChild {
    /// Write data to the child's stdin.
    fn write_stdin(&mut self, data: &[u8]) -> Result<usize, std::io::Error>;
    /// Close the child's stdin, signaling EOF.
    fn close_stdin(&mut self);
    /// Read from the child's stdout.
    fn read_stdout(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error>;
    /// Read from the child's stderr.
    fn read_stderr(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error>;
    /// Wait for the child to exit and return its exit code.
    fn wait(&mut self) -> Result<i32, std::io::Error>;
}

/// Result type for the spawn handler.
pub type SpawnResult = Result<Box<dyn ExternalChild>, SpawnError>;

/// Function type for the external command spawn handler.
pub type SpawnFn = Box<dyn Fn(&str, &[String], Vec<(String, String)>, &str) -> SpawnResult>;

thread_local! {
    static SPAWN_HANDLER: RefCell<Option<SpawnFn>> = const { RefCell::new(None) };
}

/// Set the external command spawn handler.
///
/// When the shell encounters an unknown command on WASI, it calls this handler
/// before returning "command not found".
pub fn set_spawn_handler(f: SpawnFn) {
    SPAWN_HANDLER.with(|h| {
        *h.borrow_mut() = Some(f);
    });
}

/// Try to spawn an external command via the registered handler.
///
/// Returns `None` if no handler is registered.
/// Returns `Some(Ok(child))` if the handler spawned the command.
/// Returns `Some(Err(e))` if the handler was registered but failed.
pub fn try_spawn(
    cmd: &str,
    args: &[String],
    env: Vec<(String, String)>,
    cwd: &str,
) -> Option<SpawnResult> {
    SPAWN_HANDLER.with(|h| {
        let handler = h.borrow();
        handler.as_ref().map(|f| f(cmd, args, env, cwd))
    })
}
