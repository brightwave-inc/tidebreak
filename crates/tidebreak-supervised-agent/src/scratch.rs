//! Publish bounded conversation artifacts without following links or replacing files.

use std::io::{Read, Write};
use std::path::Path;

use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt, OpenOptionsSyncExt};
#[cfg(unix)]
use cap_std::fs::OpenOptionsExt;
use cap_std::fs::{Dir, OpenOptions};
use tidebreak_core::code::supervisor_tools::{validate_artifact_path, SupervisorToolResult};

use crate::wire::SupervisorArtifact;

/// A pinned directory capability for one sandbox's conversation artifacts.
pub struct Scratch {
    root: Dir,
}

impl Scratch {
    /// Pins the workspace directory before the engine can change its path.
    pub fn open(root: &Path) -> Result<Self, String> {
        Dir::open_ambient_dir(root, cap_std::ambient_authority())
            .map(|root| Self { root })
            .map_err(|error| format!("could not open conversation scratch: {error}"))
    }

    /// Validate the whole batch, then publish each artifact once. Exact retries succeed.
    pub fn materialize(&self, artifacts: &[SupervisorArtifact]) -> Result<usize, String> {
        SupervisorToolResult {
            request_id: "scratch-validation".into(),
            output: serde_json::Value::Null,
            artifacts: artifacts.to_vec(),
        }
        .validate()?;
        for artifact in artifacts {
            self.publish(artifact)?;
        }
        Ok(artifacts.len())
    }

    fn publish(&self, artifact: &SupervisorArtifact) -> Result<(), String> {
        validate_artifact_path(&artifact.path)?;
        let mut parts = artifact.path.split('/').peekable();
        let mut parent = self.root.try_clone().map_err(|error| error.to_string())?;
        let file_name = loop {
            let part = parts.next().ok_or("artifact must name a file")?;
            if parts.peek().is_none() {
                break part;
            }
            match parent.create_dir(part) {
                Ok(()) => (),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
                Err(error) => return Err(error.to_string()),
            }
            parent = parent.open_dir_nofollow(part).map_err(|error| {
                format!("artifact parent must be a directory without symlinks: {error}")
            })?;
        };
        let temp_name = format!(".tidebreak-{}.tmp", uuid::Uuid::new_v4());
        let result = (|| -> Result<(), String> {
            let mut options = OpenOptions::new();
            options
                .write(true)
                .create_new(true)
                .nonblock(true)
                .follow(FollowSymlinks::No);
            #[cfg(unix)]
            options.mode(0o600);
            let mut file = parent
                .open_with(&temp_name, &options)
                .map_err(|error| error.to_string())?;
            file.write_all(&artifact.bytes)
                .map_err(|error| error.to_string())?;
            file.sync_all().map_err(|error| error.to_string())?;
            drop(file);
            match parent.hard_link(&temp_name, &parent, file_name) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let mut options = OpenOptions::new();
                    options.read(true).nonblock(true).follow(FollowSymlinks::No);
                    let file = parent
                        .open_with(file_name, &options)
                        .map_err(|error| error.to_string())?;
                    let metadata = file.metadata().map_err(|error| error.to_string())?;
                    if !metadata.is_file() || metadata.len() != artifact.bytes.len() as u64 {
                        return Err("artifact path already exists with different content".into());
                    }
                    let mut existing = Vec::new();
                    file.take(artifact.bytes.len() as u64 + 1)
                        .read_to_end(&mut existing)
                        .map_err(|error| error.to_string())?;
                    if existing == artifact.bytes {
                        Ok(())
                    } else {
                        Err("artifact path already exists with different content".into())
                    }
                }
                Err(error) => Err(error.to_string()),
            }
        })();
        let _ = parent.remove_file(&temp_name);
        result?;
        #[cfg(unix)]
        parent
            .open(".")
            .and_then(|directory| directory.sync_all())
            .map_err(|error| error.to_string())?;
        Ok(())
    }
}

/// Publish artifacts under one workspace; drivers should retain a pinned [`Scratch`].
pub fn materialize_artifacts(
    root: &Path,
    artifacts: &[SupervisorArtifact],
) -> Result<usize, String> {
    Scratch::open(root)?.materialize(artifacts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidebreak_core::code::supervisor_tools::MAX_ARTIFACT_BYTES;

    fn artifact(path: &str, bytes: &[u8]) -> SupervisorArtifact {
        SupervisorArtifact {
            path: path.into(),
            media_type: "text/plain".into(),
            bytes: bytes.to_vec(),
        }
    }

    #[test]
    fn publishes_nested_files_and_accepts_only_exact_retries() {
        let root = tempfile::tempdir().unwrap();
        let scratch = Scratch::open(root.path()).unwrap();
        let file = artifact("conversation/export/thread.jsonl", b"one\n");
        assert_eq!(scratch.materialize(std::slice::from_ref(&file)).unwrap(), 1);
        assert_eq!(scratch.materialize(&[file]).unwrap(), 1);
        assert!(scratch
            .materialize(&[artifact("conversation/export/thread.jsonl", b"two\n")])
            .is_err());
        assert_eq!(
            std::fs::read(root.path().join("conversation/export/thread.jsonl")).unwrap(),
            b"one\n"
        );
    }

    #[test]
    fn validates_all_paths_and_aggregate_before_writing() {
        let root = tempfile::tempdir().unwrap();
        let scratch = Scratch::open(root.path()).unwrap();
        for path in [
            "../escape",
            "/abs",
            "conversation/../up",
            "conversation/a/./b",
            "other/file",
        ] {
            assert!(scratch
                .materialize(&[
                    artifact("conversation/first", b"ok"),
                    artifact(path, b"bad")
                ])
                .is_err());
            assert!(!root.path().join("conversation").exists());
        }
        assert!(scratch
            .materialize(&[
                artifact("conversation/a", &vec![0; MAX_ARTIFACT_BYTES / 2 + 1]),
                artifact("conversation/b", &vec![0; MAX_ARTIFACT_BYTES / 2 + 1]),
            ])
            .is_err());
        assert!(!root.path().join("conversation").exists());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlink_parents_and_existing_symlink_files() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("conversation")).unwrap();
        let scratch = Scratch::open(root.path()).unwrap();
        assert!(scratch
            .materialize(&[artifact("conversation/file", b"secret")])
            .is_err());
        assert!(!outside.path().join("file").exists());
        std::fs::remove_file(root.path().join("conversation")).unwrap();
        std::fs::create_dir(root.path().join("conversation")).unwrap();
        std::fs::write(outside.path().join("file"), b"same").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("file"),
            root.path().join("conversation/file"),
        )
        .unwrap();
        assert!(scratch
            .materialize(&[artifact("conversation/file", b"same")])
            .is_err());
        assert_eq!(std::fs::read(outside.path().join("file")).unwrap(), b"same");
    }
}
