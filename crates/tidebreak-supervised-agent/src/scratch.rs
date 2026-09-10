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
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                format!("artifact {} parent could not be created: {error}", artifact.path)
            })?;
        }
        std::fs::write(&target, &artifact.bytes).map_err(|error| {
            format!("artifact {} could not be written: {error}", artifact.path)
        })?;
        written += 1;
    }
    Ok(written)
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
}
