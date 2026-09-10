//! Safe materialization of bounded host artifacts into the sandbox scratch.
//!
//! Conversation export and attachment results leave the host as bounded
//! bytes with a safe generated relative path. The agent writes them into its
//! private scratch directory so the returned paths the engine sees are real
//! files, never server-only paths pretending to work remotely.

use std::path::{Component, Path, PathBuf};

use crate::wire::SupervisorArtifact;

/// Ceiling on one artifact, matching the host contract (2 MiB).
pub const MAX_ARTIFACT_BYTES: usize = 2 * 1024 * 1024;

/// Writes each artifact under `scratch_root`, refusing any path that escapes
/// it. Returns the count materialized; a refusal is loud and prevents the
/// sandbox from reporting a fake remote path.
pub fn materialize_artifacts(
    scratch_root: &Path,
    artifacts: &[SupervisorArtifact],
) -> Result<usize, String> {
    let root = scratch_root.canonicalize().unwrap_or_else(|_| scratch_root.to_path_buf());
    let mut written = 0;
    for artifact in artifacts {
        let relative = safe_relative(&artifact.path)?;
        if artifact.bytes.len() > MAX_ARTIFACT_BYTES {
            return Err(format!(
                "artifact {} exceeds the {} byte ceiling",
                artifact.path,
                MAX_ARTIFACT_BYTES
            ));
        }
        let target = root.join(&relative);
        create_clean_parents(&root, target.parent().unwrap_or(&root)).map_err(|error| {
            format!("artifact {} parent could not be created: {error}", artifact.path)
        })?;
        match std::fs::symlink_metadata(&target) {
            Ok(_) => {
                return Err(format!(
                    "artifact {} already exists; refusing to overwrite scratch state",
                    artifact.path
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "artifact {} could not be checked: {error}",
                    artifact.path
                ));
            }
        }
        std::fs::write(&target, &artifact.bytes).map_err(|error| {
            format!("artifact {} could not be written: {error}", artifact.path)
        })?;
        written += 1;
    }
    Ok(written)
}

/// Creates parent directories one level at a time, refusing any component
/// that is (or becomes through creation) a symlink so artifact bytes can
/// never escape the scratch root.
fn create_clean_parents(root: &Path, parent: &Path) -> Result<(), String> {
    if !parent.starts_with(root) {
        return Err("artifact parent escapes scratch".to_owned());
    }
    let relative = parent.strip_prefix(root).map_err(|_| "artifact parent escapes scratch".to_owned())?;
    let mut cursor = root.to_path_buf();
    for component in relative.components() {
        cursor.push(component);
        let metadata = match std::fs::symlink_metadata(&cursor) {
            Ok(metadata) => metadata,
            Err(_) => {
                std::fs::create_dir(&cursor).map_err(|error| error.to_string())?;
                continue;
            }
        };
        if metadata.file_type().is_symlink() {
            return Err("artifact parent resolves through a symlink".to_owned());
        }
        if !metadata.is_dir() {
            return Err("artifact parent is not a directory".to_owned());
        }
    }
    Ok(())
}

/// Accepts only empty or relative, normal, non-dotdot paths.
fn safe_relative(path: &str) -> Result<PathBuf, String> {
    if path.is_empty() {
        return Err("artifact path is empty".to_owned());
    }
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        return Err(format!("artifact path is absolute: {path}"));
    }
    for component in candidate.components() {
        match component {
            Component::Normal(_) => {}
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!("artifact path escapes scratch: {path}"));
            }
        }
    }
    Ok(candidate.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifacts_write_under_scratch_and_parents_are_created() {
        let dir = tempfile::tempdir().unwrap();
        let artifacts = vec![
            SupervisorArtifact {
                path: "exports/thread.jsonl".to_owned(),
                media_type: "application/jsonl".to_owned(),
                bytes: b"{\"ok\":true}\n".to_vec(),
            },
            SupervisorArtifact {
                path: "images/photo.png".to_owned(),
                media_type: "image/png".to_owned(),
                bytes: vec![1, 2, 3],
            },
        ];
        assert_eq!(materialize_artifacts(dir.path(), &artifacts).unwrap(), 2);
        assert_eq!(
            std::fs::read(dir.path().join("exports/thread.jsonl")).unwrap(),
            b"{\"ok\":true}\n"
        );
        assert_eq!(
            std::fs::read(dir.path().join("images/photo.png")).unwrap(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn escaping_artifact_paths_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        for path in ["../escape", "/abs", "a/../../up", ".."] {
            let artifacts = vec![SupervisorArtifact {
                path: path.to_owned(),
                media_type: "text/plain".to_owned(),
                bytes: vec![],
            }];
            assert!(materialize_artifacts(dir.path(), &artifacts).is_err(), "{path}");
        }
    }

    #[test]
    fn existing_or_symlinked_parents_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join("exports");
        std::fs::create_dir(&existing).unwrap();
        std::fs::write(existing.join("thread.jsonl"), "first").unwrap();
        let overwrite = vec![SupervisorArtifact {
            path: "exports/thread.jsonl".to_owned(),
            media_type: "application/jsonl".to_owned(),
            bytes: b"second".to_vec(),
        }];
        assert!(
            materialize_artifacts(dir.path(), &overwrite)
                .unwrap_err()
                .contains("already exists")
        );
        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("payload"), "x").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, dir.path().join("link")).unwrap();
            let through_link = vec![SupervisorArtifact {
                path: "link/payload".to_owned(),
                media_type: "text/plain".to_owned(),
                bytes: vec![],
            }];
            assert!(
                materialize_artifacts(dir.path(), &through_link)
                    .unwrap_err()
                    .contains("symlink"),
                "a parent symlink must never carry artifact bytes outside scratch"
            );
        }
    }
}
