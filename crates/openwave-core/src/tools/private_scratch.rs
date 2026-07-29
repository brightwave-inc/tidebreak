//! Capability-confined private-scratch filesystem primitives.

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use cap_fs_ext::OpenOptionsSyncExt;
#[cfg(unix)]
use cap_std::fs::OpenOptionsExt;
use cap_std::fs::{Dir, OpenOptions};

pub(super) const MAX_READ_FILE_BYTES: usize = 64 * 1024;
const MAX_LIST_DIR_BYTES: usize = 64 * 1024;
const MAX_LIST_DIR_ENTRIES: usize = 4_096;

/// Validate a scratch-relative path before handing it to the capability API.
pub(super) fn relative_path(rel: &str) -> std::result::Result<PathBuf, String> {
    let path = Path::new(rel);
    if path.is_absolute() {
        return Err(format!("path must be relative to private scratch: {rel}"));
    }
    for component in path.components() {
        match component {
            Component::ParentDir => return Err(format!("path may not contain `..`: {rel}")),
            Component::Prefix(_) | Component::RootDir => {
                return Err(format!("path must be relative to private scratch: {rel}"));
            }
            _ => {}
        }
    }
    Ok(path.to_path_buf())
}

/// The last segment of a scratch-relative path, for a card row's name.
///
/// A row leads with the file rather than the path so a column of results stays
/// scannable; the full path is the row's secondary hint. A path that ends in a
/// separator has no last segment, and there is nothing better to show than what
/// the model asked for.
pub(super) fn file_name(path: &str) -> &str {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
}

/// How many lines a read returned, phrased for a card.
pub(super) fn line_count(content: &str) -> String {
    // A file with no trailing newline still has a last line, and an empty file
    // has none — which `lines()` gets right and a newline count does not.
    let lines = content.lines().count();
    format!("{lines} {}", if lines == 1 { "line" } else { "lines" })
}

pub(super) fn read_utf8_file(workspace: &Dir, path: &Path) -> std::result::Result<String, String> {
    let bytes = read_regular_file_bytes(workspace, path, MAX_READ_FILE_BYTES)?;
    String::from_utf8(bytes).map_err(|_| "file is not valid UTF-8".into())
}

/// Read a bounded regular file without following a caller-controlled path.
pub(super) fn read_regular_file_bytes(
    workspace: &Dir,
    path: &Path,
    max_bytes: usize,
) -> std::result::Result<Vec<u8>, String> {
    let mut options = OpenOptions::new();
    options.read(true).nonblock(true);
    let file = workspace
        .open_with(path, &options)
        .map_err(|error| error.to_string())?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err("path is not a regular file".into());
    }
    if metadata.len() > max_bytes as u64 {
        return Err(format!("file is too large (maximum {max_bytes} bytes)"));
    }

    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take((max_bytes + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > max_bytes {
        return Err(format!("file is too large (maximum {max_bytes} bytes)"));
    }
    Ok(bytes)
}

pub(super) fn list_directory(workspace: &Dir, path: &Path) -> std::result::Result<String, String> {
    let entries = workspace
        .read_dir(path)
        .map_err(|error| error.to_string())?;
    let mut names = Vec::new();
    let mut output_bytes = 0usize;
    for entry in entries {
        if names.len() == MAX_LIST_DIR_ENTRIES {
            return Err(format!(
                "directory has too many entries (maximum {MAX_LIST_DIR_ENTRIES})"
            ));
        }
        let entry = entry.map_err(|error| error.to_string())?;
        let mut name = entry.file_name().to_string_lossy().into_owned();
        if entry
            .file_type()
            .map(|file_type| file_type.is_dir())
            .unwrap_or(false)
        {
            name.push('/');
        }
        output_bytes = output_bytes.saturating_add(name.len() + usize::from(!names.is_empty()));
        if output_bytes > MAX_LIST_DIR_BYTES {
            return Err(format!(
                "directory listing is too large (maximum {MAX_LIST_DIR_BYTES} bytes)"
            ));
        }
        names.push(name);
    }
    names.sort();
    Ok(names.join("\n"))
}

pub(super) fn write_utf8_file(workspace: &Dir, path: &Path, content: &[u8]) -> std::io::Result<()> {
    let parent_path = path.parent().unwrap_or_else(|| Path::new(""));
    workspace.create_dir_all(parent_path)?;
    let parent = if parent_path.as_os_str().is_empty() {
        workspace.try_clone()?
    } else {
        workspace.open_dir(parent_path)?
    };
    let file_name = path.file_name().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path must name a file")
    })?;
    let permissions = match parent.symlink_metadata(file_name) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "path is not a regular file",
            ));
        }
        Ok(metadata) => Some(metadata.permissions()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };

    let temp_name = format!(".openwave-{}.tmp", uuid::Uuid::new_v4());
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true).nonblock(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = parent.open_with(&temp_name, &options)?;
        if !file.metadata()?.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "temporary path is not a regular file",
            ));
        }
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)?;
        }
        file.write_all(content)?;
        file.sync_all()?;
        drop(file);
        parent.rename(&temp_name, &parent, file_name)?;
        #[cfg(unix)]
        parent.open(".")?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = parent.remove_file(&temp_name);
    }
    result
}

/// Publish bytes at a write-once path without replacing an earlier revision.
///
/// A hard link gives the temporary file an atomic, no-replace final name. If a
/// previous attempt already published that name, accepting it only when its
/// exact bytes match makes an interrupted store response safe to retry.
pub(crate) fn publish_immutable_file(
    workspace: &Dir,
    path: &Path,
    content: &[u8],
) -> std::result::Result<(), String> {
    let parent_path = path.parent().unwrap_or_else(|| Path::new(""));
    workspace
        .create_dir_all(parent_path)
        .map_err(|error| error.to_string())?;
    let parent = if parent_path.as_os_str().is_empty() {
        workspace.try_clone()
    } else {
        workspace.open_dir(parent_path)
    }
    .map_err(|error| error.to_string())?;
    let file_name = path
        .file_name()
        .ok_or_else(|| "path must name a file".to_string())?;
    let temp_name = format!(".openwave-{}.tmp", uuid::Uuid::new_v4());

    let write_temp = || -> std::io::Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true).nonblock(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = parent.open_with(&temp_name, &options)?;
        if !file.metadata()?.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "temporary path is not a regular file",
            ));
        }
        file.write_all(content)?;
        file.sync_all()?;
        Ok(())
    };

    if let Err(error) = write_temp() {
        let _ = parent.remove_file(&temp_name);
        return Err(error.to_string());
    }

    match parent.hard_link(&temp_name, &parent, file_name) {
        Ok(()) => {
            let _ = parent.remove_file(&temp_name);
            #[cfg(unix)]
            parent
                .open(".")
                .and_then(|directory| directory.sync_all())
                .map_err(|error| format!("could not sync immutable revision directory: {error}"))?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let _ = parent.remove_file(&temp_name);
            let metadata = parent
                .symlink_metadata(file_name)
                .map_err(|read_error| read_error.to_string())?;
            if !metadata.file_type().is_file() || metadata.len() != content.len() as u64 {
                return Err("immutable revision path already exists with different content".into());
            }
            let mut options = OpenOptions::new();
            options.read(true).nonblock(true);
            let mut file = parent
                .open_with(file_name, &options)
                .map_err(|read_error| read_error.to_string())?;
            let mut existing = Vec::with_capacity(content.len());
            file.read_to_end(&mut existing)
                .map_err(|read_error| read_error.to_string())?;
            if existing == content {
                Ok(())
            } else {
                Err("immutable revision path already exists with different content".into())
            }
        }
        Err(error) => {
            let _ = parent.remove_file(&temp_name);
            Err(error.to_string())
        }
    }
}
