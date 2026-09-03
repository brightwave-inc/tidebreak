//! Crash-safe file replacement: write a sibling temporary, fsync, then
//! replace the destination and fsync its directory.
//!
//! Unix `rename` is atomic. Windows `rename` is not when the destination
//! exists, so the Windows path uses `MoveFileExW` with replace and write-
//! through. Other platforms return [`std::io::ErrorKind::Unsupported`].

use std::io;
use std::path::Path;

/// Atomically move `temporary` over `destination`.
pub fn replace_file(temporary: &Path, destination: &Path) -> io::Result<()> {
    replace_file_inner(temporary, destination)
}

/// Fsync `directory` so a preceding rename is durable. A no-op on Windows:
/// opening a directory for write-through is not meaningful there.
pub fn sync_directory(directory: &Path) -> io::Result<()> {
    sync_directory_inner(directory)
}

#[cfg(unix)]
fn replace_file_inner(temporary: &Path, destination: &Path) -> io::Result<()> {
    std::fs::rename(temporary, destination)
}

#[cfg(windows)]
fn replace_file_inner(temporary: &Path, destination: &Path) -> io::Result<()> {
    replace_file_windows(temporary, destination)
}

#[cfg(not(any(unix, windows)))]
fn replace_file_inner(_temporary: &Path, _destination: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic file replacement is unsupported on this platform",
    ))
}

#[cfg(all(windows, feature = "blob-fs"))]
fn replace_file_windows(temporary: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let temporary = temporary
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let succeeded = unsafe {
        MoveFileExW(
            temporary.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if succeeded == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(all(windows, not(feature = "blob-fs")))]
fn replace_file_windows(temporary: &Path, destination: &Path) -> io::Result<()> {
    std::fs::rename(temporary, destination)
}

#[cfg(unix)]
fn sync_directory_inner(directory: &Path) -> io::Result<()> {
    std::fs::File::open(directory)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory_inner(_directory: &Path) -> io::Result<()> {
    Ok(())
}
