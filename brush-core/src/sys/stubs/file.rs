//! Cloneable file wrapper for WASM platforms.
//!
//! Since std::fs::File::try_clone() is not supported on WASM,
//! this module provides a wrapper that uses Arc<Mutex<>> to allow
//! multiple "handles" to share the same underlying file.

use std::io::{self, Read, Seek, SeekFrom, Write};
use std::sync::{Arc, Mutex};

/// A cloneable file wrapper for WASM.
///
/// This wraps a std::fs::File in Arc<Mutex<>> to allow cloning.
/// Each clone maintains its own seek position.
pub struct CloneableFile {
    inner: Arc<Mutex<std::fs::File>>,
    /// Each handle tracks its own position
    position: u64,
}

impl CloneableFile {
    /// Create a new CloneableFile from a std::fs::File.
    pub fn new(file: std::fs::File) -> Self {
        Self {
            inner: Arc::new(Mutex::new(file)),
            position: 0,
        }
    }

    /// Try to clone this file handle.
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            inner: Arc::clone(&self.inner),
            position: self.position,
        })
    }

    /// Get file metadata.
    pub fn metadata(&self) -> io::Result<std::fs::Metadata> {
        let file = self
            .inner
            .lock()
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "mutex poisoned"))?;
        file.metadata()
    }

    /// Check if this is a directory.
    pub fn is_dir(&self) -> bool {
        self.metadata().map(|m| m.is_dir()).unwrap_or(false)
    }

    /// Check if this is a terminal (always false on WASM).
    pub fn is_terminal(&self) -> bool {
        false
    }
}

impl Read for CloneableFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut file = self
            .inner
            .lock()
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "mutex poisoned"))?;

        // Seek to our position
        file.seek(SeekFrom::Start(self.position))?;

        // Read
        let n = file.read(buf)?;

        // Update our position
        self.position += n as u64;

        Ok(n)
    }
}

impl Write for CloneableFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut file = self
            .inner
            .lock()
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "mutex poisoned"))?;

        // Seek to our position
        file.seek(SeekFrom::Start(self.position))?;

        // Write
        let n = file.write(buf)?;

        // Update our position
        self.position += n as u64;

        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut file = self
            .inner
            .lock()
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "mutex poisoned"))?;
        file.flush()
    }
}

impl From<std::fs::File> for CloneableFile {
    fn from(file: std::fs::File) -> Self {
        Self::new(file)
    }
}
