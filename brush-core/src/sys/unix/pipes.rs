//! Unix pipe implementation using std::io::pipe().

use std::io;

pub(crate) type PipeReader = std::io::PipeReader;
pub(crate) type PipeWriter = std::io::PipeWriter;

/// Creates a new pipe using the standard library.
pub(crate) fn pipe() -> io::Result<(PipeReader, PipeWriter)> {
    std::io::pipe()
}
