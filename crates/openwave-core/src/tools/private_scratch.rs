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

pub(super) fn read_utf8_file(workspace: &Dir, path: &Path) -> std::result::Result<String, String> {
    let mut options = OpenOptions::new();
    options.read(true).nonblock(true);
    let file = workspace
        .open_with(path, &options)
        .map_err(|error| error.to_string())?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err("path is not a regular file".into());
    }
    if metadata.len() > MAX_READ_FILE_BYTES as u64 {
        return Err(format!(
            "file is too large (maximum {MAX_READ_FILE_BYTES} bytes)"
        ));
    }

    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take((MAX_READ_FILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > MAX_READ_FILE_BYTES {
        return Err(format!(
            "file is too large (maximum {MAX_READ_FILE_BYTES} bytes)"
        ));
    }
    String::from_utf8(bytes).map_err(|_| "file is not valid UTF-8".into())
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
