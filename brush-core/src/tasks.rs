//! Tasks the shell runs concurrently with itself.

use std::{
    pin::Pin,
    task::{Context, Poll},
};

use crate::error;

/// A future that yields a task's output when it finishes.
type TaskFuture<T> = Pin<Box<dyn Future<Output = Result<T, error::Error>> + Send + Sync>>;

/// Tracks a task started on the shell's behalf. Awaiting it yields the task's
/// output, or an error if the task couldn't run to completion (e.g., it
/// panicked).
///
/// Dropping the handle doesn't stop the task; [`TaskHandle::abort`] does, if
/// whoever started it said how.
pub struct TaskHandle<T> {
    future: TaskFuture<T>,
    abort: Option<Box<dyn Fn() + Send + Sync>>,
}

impl<T> TaskHandle<T> {
    /// Tracks a task through a future that yields its output when it finishes.
    ///
    /// # Arguments
    ///
    /// * `future` - The future to await for the task's output.
    pub fn from_future(
        future: impl Future<Output = Result<T, error::Error>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            future: Box::pin(future),
            abort: None,
        }
    }

    /// Tracks a task through a future that yields its output when it finishes,
    /// and a function that stops it. Once stopped, the future should yield an
    /// error.
    ///
    /// # Arguments
    ///
    /// * `future` - The future to await for the task's output.
    /// * `abort` - Stops the task.
    pub fn from_future_with_abort(
        future: impl Future<Output = Result<T, error::Error>> + Send + Sync + 'static,
        abort: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        Self {
            future: Box::pin(future),
            abort: Some(Box::new(abort)),
        }
    }

    /// Stops the task, if it can be stopped; returns whether it could.
    pub fn abort(&self) -> bool {
        self.abort.as_ref().map(|abort| abort()).is_some()
    }
}

impl<T> Future for TaskHandle<T> {
    type Output = Result<T, error::Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.future.as_mut().poll(cx)
    }
}
