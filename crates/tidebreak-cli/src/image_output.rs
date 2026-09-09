//! Private screenshot files shared by browser and native CLI commands.

use tidebreak_core::{AgentError, Result};
use uuid::Uuid;

/// Write image bytes to `path` privately: created 0600 on Unix, replaced
/// atomically via a same-directory temp file, never following a symlink at
/// the destination.
pub(crate) fn write_image_private(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write as _;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() {
            return Err(AgentError::msg("--output path is a symlink"));
        }
    }
    let tmp = parent.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("computer-image"),
        Uuid::new_v4().simple()
    ));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options
            .open(&tmp)
            .map_err(|error| AgentError::msg(format!("could not create image file: {error}")))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|error| AgentError::msg(format!("could not set image mode: {error}")))?;
        }
        file.write_all(bytes)
            .map_err(|error| AgentError::msg(format!("could not write image file: {error}")))?;
        file.sync_all()
            .map_err(|error| AgentError::msg(format!("could not sync image file: {error}")))?;
        drop(file);
        std::fs::rename(&tmp, path)
            .map_err(|error| AgentError::msg(format!("could not install image file: {error}")))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}
