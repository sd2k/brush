//! In-memory pipe implementation for WASI and other platforms without OS pipes.
//!
//! This provides a working pipe implementation using shared memory buffers,
//! allowing pipelines to work in environments where std::io::pipe() is not available.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex};

/// Shared state between pipe reader and writer.
struct PipeBuffer {
    data: VecDeque<u8>,
    closed: bool,
}

impl PipeBuffer {
    fn new() -> Self {
        Self {
            data: VecDeque::with_capacity(8192),
            closed: false,
        }
    }
}

/// In-memory pipe reader.
#[derive(Clone)]
pub struct PipeReader {
    buffer: Arc<Mutex<PipeBuffer>>,
}

impl PipeReader {
    /// Tries to clone the reader.
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            buffer: Arc::clone(&self.buffer),
        })
    }
}

impl From<PipeReader> for std::process::Stdio {
    fn from(_reader: PipeReader) -> Self {
        // Cannot convert in-memory pipe to process Stdio
        Self::null()
    }
}

impl Read for PipeReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut buffer = self.buffer.lock().unwrap();

        if buffer.data.is_empty() {
            if buffer.closed {
                // Writer closed, return EOF
                return Ok(0);
            }
            // No data available but writer still open - return 0
            return Ok(0);
        }

        // Read available data
        let to_read = buf.len().min(buffer.data.len());
        for (i, byte) in buffer.data.drain(..to_read).enumerate() {
            buf[i] = byte;
        }

        Ok(to_read)
    }
}

/// In-memory pipe writer.
pub struct PipeWriter {
    buffer: Arc<Mutex<PipeBuffer>>,
}

impl Clone for PipeWriter {
    fn clone(&self) -> Self {
        Self {
            buffer: Arc::clone(&self.buffer),
        }
    }
}

impl PipeWriter {
    /// Tries to clone the writer.
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            buffer: Arc::clone(&self.buffer),
        })
    }
}

impl From<PipeWriter> for std::process::Stdio {
    fn from(_writer: PipeWriter) -> Self {
        // Cannot convert in-memory pipe to process Stdio
        Self::null()
    }
}

impl Write for PipeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut buffer = self.buffer.lock().unwrap();

        if buffer.closed {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "pipe closed"));
        }

        buffer.data.extend(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for PipeWriter {
    fn drop(&mut self) {
        // When the last writer is dropped, mark the pipe as closed
        if Arc::strong_count(&self.buffer) <= 2 {
            if let Ok(mut buffer) = self.buffer.lock() {
                buffer.closed = true;
            }
        }
    }
}

/// Creates a new in-memory pipe.
pub fn pipe() -> io::Result<(PipeReader, PipeWriter)> {
    let buffer = Arc::new(Mutex::new(PipeBuffer::new()));

    let reader = PipeReader {
        buffer: Arc::clone(&buffer),
    };
    let writer = PipeWriter { buffer };

    Ok((reader, writer))
}
