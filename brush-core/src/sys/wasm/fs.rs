//! Filesystem utilities for WASM.

pub use crate::sys::stubs::fs::*;

// WASI has no permissions (or setuid, setgid and sticky bits) to check, but a
// path must at least exist.
impl crate::sys::fs::PathExt for std::path::Path {
    fn readable(&self) -> bool {
        self.exists()
    }

    fn writable(&self) -> bool {
        self.exists()
    }

    fn executable(&self) -> bool {
        self.exists()
    }

    fn exists_and_is_block_device(&self) -> bool {
        file_type_is(self, FileKind::BlockDevice)
    }

    fn exists_and_is_char_device(&self) -> bool {
        file_type_is(self, FileKind::CharDevice)
    }

    fn exists_and_is_fifo(&self) -> bool {
        file_type_is(self, FileKind::Fifo)
    }

    fn exists_and_is_socket(&self) -> bool {
        file_type_is(self, FileKind::Socket)
    }

    fn exists_and_is_setgid(&self) -> bool {
        false
    }

    fn exists_and_is_setuid(&self) -> bool {
        false
    }

    fn exists_and_is_sticky_bit(&self) -> bool {
        false
    }

    fn get_device_and_inode(&self) -> Result<(u64, u64), crate::error::Error> {
        Ok((0, 0))
    }
}

/// File types other than files, directories and symlinks, which std's
/// portable `FileType` can't tell apart (`std::os::wasi::fs::FileTypeExt` is
/// unstable).
#[derive(Clone, Copy)]
enum FileKind {
    BlockDevice,
    CharDevice,
    Fifo,
    Socket,
}

/// Returns whether `path` exists and is of the given kind, as `stat` reports it
/// (WASI's descriptor types, as wasi-libc maps them; a fifo is never one).
#[cfg(target_os = "wasi")]
fn file_type_is(path: &std::path::Path, kind: FileKind) -> bool {
    let Some(path) = path.to_str().and_then(|p| std::ffi::CString::new(p).ok()) else {
        return false;
    };
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `path` is a NUL-terminated string, and `st` has room for a `stat`.
    if unsafe { libc::stat(path.as_ptr(), st.as_mut_ptr()) } != 0 {
        return false;
    }
    // SAFETY: `stat` succeeded, so it filled in `st`.
    let mode = unsafe { st.assume_init() }.st_mode;
    let want = match kind {
        FileKind::BlockDevice => libc::S_IFBLK,
        FileKind::CharDevice => libc::S_IFCHR,
        FileKind::Fifo => libc::S_IFIFO,
        FileKind::Socket => libc::S_IFSOCK,
    };
    mode & libc::S_IFMT == want
}

/// Without WASI (e.g. `wasm32-unknown-unknown`) there's no `stat` to ask.
#[cfg(not(target_os = "wasi"))]
const fn file_type_is(_path: &std::path::Path, _kind: FileKind) -> bool {
    false
}

/// Splits a PATH-like value into individual paths.
///
/// On WASM, `std::env::split_paths` is not available, so this
/// implementation splits by the `:` separator.
pub fn split_paths<T: AsRef<std::ffi::OsStr> + ?Sized>(
    s: &T,
) -> impl Iterator<Item = std::path::PathBuf> {
    s.as_ref()
        .to_str()
        .unwrap_or_default()
        .split(':')
        .map(std::path::PathBuf::from)
        .collect::<Vec<_>>()
        .into_iter()
}
