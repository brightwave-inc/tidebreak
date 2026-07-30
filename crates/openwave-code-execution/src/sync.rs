//! Mirror a host directory into a durable workspace and back.
//!
//! Remote sandboxes have their own filesystem, but the model is shown one
//! path vocabulary: the file tools and `exec` both speak private-scratch
//! relative paths. The host keeps that story true by pushing the chat's
//! scratch into the workspace before a command runs and pulling the workspace
//! back afterwards, so a file written by either side is visible to the other.
//!
//! The mirror is additive in both directions: a file deleted on one side is
//! not deleted on the other. Everything a sync leaves behind — oversized
//! files, dependency trees, truncated listings — is reported in the
//! [`SyncReport`], never skipped silently.

use std::path::{Path, PathBuf};

use crate::{
    CodeExecutionError, ExecutionWorkspaceId, WorkspaceFilePath, WorkspaceLifecycle,
    MAX_WORKSPACE_FILE_BYTES,
};

/// The most files one direction of a sync will transfer before stopping and
/// saying so. A workspace that grows a dependency tree past the skip list
/// should degrade into a bounded transfer plus a note, not an unbounded copy.
pub const MAX_SYNC_FILES: usize = 256;

/// Directory names never mirrored in either direction: version control and
/// dependency trees that are large, regenerable, and meaningless to copy
/// between the host and a sandbox.
const SKIPPED_DIRS: &[&str] = &[".git", ".venv", "__pycache__", "node_modules"];

/// What one direction of a sync transferred, and everything it left behind.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncReport {
    pub transferred: usize,
    pub notes: Vec<String>,
}

impl SyncReport {
    fn skip(&mut self, note: impl Into<String>) {
        self.notes.push(note.into());
    }
}

/// Push every eligible file under `host_dir` into the workspace.
///
/// A missing `host_dir` pushes nothing: the chat simply has no scratch yet.
pub async fn push_host_dir(
    lifecycle: &dyn WorkspaceLifecycle,
    workspace: &ExecutionWorkspaceId,
    host_dir: &Path,
) -> Result<SyncReport, CodeExecutionError> {
    let mut report = SyncReport::default();
    if tokio::fs::symlink_metadata(host_dir).await.is_err() {
        return Ok(report);
    }
    let mut stack: Vec<(PathBuf, String)> = vec![(host_dir.to_path_buf(), String::new())];
    let mut files: Vec<(WorkspaceFilePath, PathBuf)> = Vec::new();
    while let Some((dir, prefix)) = stack.pop() {
        let mut entries = tokio::fs::read_dir(&dir)
            .await
            .map_err(|_| unreadable(&prefix))?;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|_| unreadable(&prefix))?
        {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                report.skip(format!(
                    "not pushed: an entry under '{prefix}/' has a non-UTF-8 name"
                ));
                continue;
            };
            let relative = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let metadata = tokio::fs::symlink_metadata(entry.path())
                .await
                .map_err(|_| unreadable(&relative))?;
            if metadata.is_symlink() {
                report.skip(format!("not pushed: {relative} is a symlink"));
                continue;
            }
            if metadata.is_dir() {
                if SKIPPED_DIRS.contains(&name.as_str()) {
                    report.skip(format!("not pushed: {relative}/ (dependency or VCS tree)"));
                } else {
                    stack.push((entry.path(), relative));
                }
                continue;
            }
            if !metadata.is_file() {
                continue;
            }
            if metadata.len() > MAX_WORKSPACE_FILE_BYTES as u64 {
                report.skip(format!(
                    "not pushed: {relative} exceeds the {MAX_WORKSPACE_FILE_BYTES}-byte file limit"
                ));
                continue;
            }
            let Ok(path) = WorkspaceFilePath::parse(&relative) else {
                report.skip(format!(
                    "not pushed: {relative} is not a valid workspace path"
                ));
                continue;
            };
            files.push((path, entry.path()));
        }
    }
    files.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
    let overflow = files.len().saturating_sub(MAX_SYNC_FILES);
    if overflow > 0 {
        report.skip(format!(
            "not pushed: {overflow} more file(s) beyond the {MAX_SYNC_FILES}-file sync limit"
        ));
    }
    for (path, host_path) in files.into_iter().take(MAX_SYNC_FILES) {
        let content = tokio::fs::read(&host_path)
            .await
            .map_err(|_| unreadable(path.as_str()))?;
        lifecycle
            .put_workspace_file(workspace, &path, &content)
            .await?;
        report.transferred += 1;
    }
    Ok(report)
}

/// Pull every eligible workspace file into `host_dir`, writing only files
/// whose content actually differs from the host copy.
pub async fn pull_into_host_dir(
    lifecycle: &dyn WorkspaceLifecycle,
    workspace: &ExecutionWorkspaceId,
    host_dir: &Path,
) -> Result<SyncReport, CodeExecutionError> {
    let mut report = SyncReport::default();
    let mut stack: Vec<Option<WorkspaceFilePath>> = vec![None];
    let mut files: Vec<WorkspaceFilePath> = Vec::new();
    while let Some(dir) = stack.pop() {
        let listing = lifecycle
            .list_workspace_files(workspace, dir.as_ref())
            .await?;
        if listing.truncated {
            let shown = dir
                .as_ref()
                .map_or("the workspace root", WorkspaceFilePath::as_str);
            report.skip(format!(
                "not fully pulled: the listing of {shown} was truncated"
            ));
        }
        for entry in listing.entries {
            // The path comes back from the sandbox; re-validate it so a
            // hostile or confused backend cannot steer a write outside the
            // chat's scratch directory.
            let Ok(path) = WorkspaceFilePath::parse(&entry.path) else {
                report.skip(format!(
                    "not pulled: '{}' is not a valid workspace path",
                    entry.path
                ));
                continue;
            };
            if entry.directory {
                if SKIPPED_DIRS.contains(&path.file_name()) {
                    report.skip(format!(
                        "not pulled: {}/ (dependency or VCS tree)",
                        path.as_str()
                    ));
                } else {
                    stack.push(Some(path));
                }
                continue;
            }
            if entry
                .size_bytes
                .is_some_and(|size| size > MAX_WORKSPACE_FILE_BYTES as u64)
            {
                report.skip(format!(
                    "not pulled: {} exceeds the {MAX_WORKSPACE_FILE_BYTES}-byte file limit",
                    path.as_str()
                ));
                continue;
            }
            files.push(path);
        }
    }
    files.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    let overflow = files.len().saturating_sub(MAX_SYNC_FILES);
    if overflow > 0 {
        report.skip(format!(
            "not pulled: {overflow} more file(s) beyond the {MAX_SYNC_FILES}-file sync limit"
        ));
    }
    for path in files.into_iter().take(MAX_SYNC_FILES) {
        let content = lifecycle.get_workspace_file(workspace, &path).await?;
        let host_path = host_dir.join(path.as_str());
        if let Ok(existing) = tokio::fs::read(&host_path).await {
            if existing == content {
                continue;
            }
        }
        if tokio::fs::symlink_metadata(&host_path)
            .await
            .is_ok_and(|metadata| metadata.is_symlink())
        {
            report.skip(format!(
                "not pulled: {} is a symlink on the host",
                path.as_str()
            ));
            continue;
        }
        if let Some(parent) = host_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|_| unwritable(path.as_str()))?;
        }
        tokio::fs::write(&host_path, &content)
            .await
            .map_err(|_| unwritable(path.as_str()))?;
        report.transferred += 1;
    }
    Ok(report)
}

fn unreadable(path: &str) -> CodeExecutionError {
    CodeExecutionError::Sandbox(format!("private scratch entry '{path}' is unreadable"))
}

fn unwritable(path: &str) -> CodeExecutionError {
    CodeExecutionError::Sandbox(format!("private scratch entry '{path}' is unwritable"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{WorkspaceFileEntry, WorkspaceListing};
    use async_trait::async_trait;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    /// In-memory workspace: a flat map of normalized relative paths to bytes,
    /// listing direct children per directory the way real backends do.
    #[derive(Default)]
    struct FakeWorkspace {
        files: Mutex<BTreeMap<String, Vec<u8>>>,
        /// Extra listing rows returned verbatim, for hostile-backend cases.
        planted: Mutex<Vec<WorkspaceFileEntry>>,
    }

    impl FakeWorkspace {
        fn insert(&self, path: &str, content: &[u8]) {
            self.files
                .lock()
                .unwrap()
                .insert(path.into(), content.to_vec());
        }

        fn get(&self, path: &str) -> Option<Vec<u8>> {
            self.files.lock().unwrap().get(path).cloned()
        }
    }

    #[async_trait]
    impl WorkspaceLifecycle for FakeWorkspace {
        async fn create_workspace(
            &self,
            _workspace: &ExecutionWorkspaceId,
        ) -> Result<(), CodeExecutionError> {
            Ok(())
        }

        async fn connect_workspace(
            &self,
            _workspace: &ExecutionWorkspaceId,
        ) -> Result<bool, CodeExecutionError> {
            Ok(true)
        }

        async fn destroy_workspace(
            &self,
            _workspace: &ExecutionWorkspaceId,
        ) -> Result<(), CodeExecutionError> {
            Ok(())
        }

        async fn put_workspace_file(
            &self,
            _workspace: &ExecutionWorkspaceId,
            path: &WorkspaceFilePath,
            content: &[u8],
        ) -> Result<(), CodeExecutionError> {
            self.insert(path.as_str(), content);
            Ok(())
        }

        async fn get_workspace_file(
            &self,
            _workspace: &ExecutionWorkspaceId,
            path: &WorkspaceFilePath,
        ) -> Result<Vec<u8>, CodeExecutionError> {
            self.get(path.as_str())
                .ok_or_else(|| CodeExecutionError::Sandbox("missing file".into()))
        }

        async fn list_workspace_files(
            &self,
            _workspace: &ExecutionWorkspaceId,
            path: Option<&WorkspaceFilePath>,
        ) -> Result<WorkspaceListing, CodeExecutionError> {
            let prefix = path.map_or(String::new(), |dir| format!("{}/", dir.as_str()));
            let mut entries: Vec<WorkspaceFileEntry> = Vec::new();
            let mut seen_dirs: Vec<String> = Vec::new();
            for (file, content) in self.files.lock().unwrap().iter() {
                let Some(rest) = file.strip_prefix(&prefix) else {
                    continue;
                };
                match rest.split_once('/') {
                    None => entries.push(WorkspaceFileEntry {
                        path: file.clone(),
                        directory: false,
                        size_bytes: Some(content.len() as u64),
                    }),
                    Some((child, _)) => {
                        let child_path = format!("{prefix}{child}");
                        if !seen_dirs.contains(&child_path) {
                            seen_dirs.push(child_path.clone());
                            entries.push(WorkspaceFileEntry {
                                path: child_path,
                                directory: true,
                                size_bytes: None,
                            });
                        }
                    }
                }
            }
            if path.is_none() {
                entries.extend(self.planted.lock().unwrap().drain(..));
            }
            Ok(WorkspaceListing {
                entries,
                truncated: false,
            })
        }
    }

    fn workspace_id() -> ExecutionWorkspaceId {
        ExecutionWorkspaceId::parse("chat-1".to_owned()).unwrap()
    }

    #[tokio::test]
    async fn round_trips_host_scratch_through_the_workspace() {
        let host = tempfile::tempdir().unwrap();
        std::fs::write(host.path().join("a.txt"), "host a").unwrap();
        std::fs::create_dir_all(host.path().join("sub")).unwrap();
        std::fs::write(host.path().join("sub/b.txt"), "host b").unwrap();
        std::fs::create_dir_all(host.path().join(".git")).unwrap();
        std::fs::write(host.path().join(".git/config"), "never").unwrap();

        let fake = FakeWorkspace::default();
        let pushed = push_host_dir(&fake, &workspace_id(), host.path())
            .await
            .unwrap();
        assert_eq!(pushed.transferred, 2);
        assert_eq!(fake.get("a.txt").unwrap(), b"host a");
        assert_eq!(fake.get("sub/b.txt").unwrap(), b"host b");
        assert!(fake.get(".git/config").is_none());
        assert_eq!(
            pushed.notes,
            vec!["not pushed: .git/ (dependency or VCS tree)"]
        );

        // The sandbox edits one file and creates another; the pull mirrors
        // both back and leaves the unchanged file alone.
        fake.insert("a.txt", b"sandbox a");
        fake.insert("out/c.txt", b"sandbox c");
        let pulled = pull_into_host_dir(&fake, &workspace_id(), host.path())
            .await
            .unwrap();
        assert_eq!(pulled.transferred, 2, "{:?}", pulled.notes);
        assert_eq!(
            std::fs::read(host.path().join("a.txt")).unwrap(),
            b"sandbox a"
        );
        assert_eq!(
            std::fs::read(host.path().join("out/c.txt")).unwrap(),
            b"sandbox c"
        );
        assert_eq!(
            std::fs::read(host.path().join("sub/b.txt")).unwrap(),
            b"host b"
        );
    }

    #[tokio::test]
    async fn pull_rejects_paths_that_escape_the_host_directory() {
        let parent = tempfile::tempdir().unwrap();
        let host = parent.path().join("scratch");
        std::fs::create_dir_all(&host).unwrap();

        let fake = FakeWorkspace::default();
        fake.planted.lock().unwrap().push(WorkspaceFileEntry {
            path: "../escape.txt".into(),
            directory: false,
            size_bytes: Some(4),
        });
        fake.planted.lock().unwrap().push(WorkspaceFileEntry {
            path: "big.bin".into(),
            directory: false,
            size_bytes: Some(MAX_WORKSPACE_FILE_BYTES as u64 + 1),
        });

        let pulled = pull_into_host_dir(&fake, &workspace_id(), &host)
            .await
            .unwrap();
        assert_eq!(pulled.transferred, 0);
        assert!(!parent.path().join("escape.txt").exists());
        assert_eq!(pulled.notes.len(), 2, "{:?}", pulled.notes);
        assert!(pulled.notes[0].contains("not a valid workspace path"));
        assert!(pulled.notes[1].contains("file limit"));
    }

    #[tokio::test]
    async fn preview_files_are_mirrored_and_keep_the_per_file_cap() {
        let host = tempfile::tempdir().unwrap();
        let fake = FakeWorkspace::default();
        fake.insert("preview/overview.png", b"pixels");
        fake.planted.lock().unwrap().push(WorkspaceFileEntry {
            path: "preview/too-large.png".into(),
            directory: false,
            size_bytes: Some(MAX_WORKSPACE_FILE_BYTES as u64 + 1),
        });

        let pulled = pull_into_host_dir(&fake, &workspace_id(), host.path())
            .await
            .unwrap();

        assert_eq!(
            std::fs::read(host.path().join("preview/overview.png")).unwrap(),
            b"pixels"
        );
        assert_eq!(pulled.transferred, 1);
        assert!(pulled
            .notes
            .iter()
            .any(|note| { note.contains("preview/too-large.png") && note.contains("file limit") }));
    }

    #[tokio::test]
    async fn attached_documents_are_pushed_with_the_per_file_cap() {
        let host = tempfile::tempdir().unwrap();
        let documents = host.path().join("documents");
        std::fs::create_dir_all(&documents).unwrap();
        std::fs::write(documents.join("brief.pdf"), b"pdf bytes").unwrap();
        let oversized = std::fs::File::create(documents.join("oversized.pdf")).unwrap();
        oversized
            .set_len(MAX_WORKSPACE_FILE_BYTES as u64 + 1)
            .unwrap();
        let fake = FakeWorkspace::default();

        let pushed = push_host_dir(&fake, &workspace_id(), host.path())
            .await
            .unwrap();

        assert_eq!(fake.get("documents/brief.pdf").unwrap(), b"pdf bytes");
        assert!(fake.get("documents/oversized.pdf").is_none());
        assert_eq!(pushed.transferred, 1);
        assert!(pushed.notes.iter().any(|note| {
            note.contains("documents/oversized.pdf") && note.contains("file limit")
        }));
    }

    #[tokio::test]
    async fn bundled_document_helpers_are_not_on_the_sync_skip_list() {
        let host = tempfile::tempdir().unwrap();
        let helper = host
            .path()
            .join(crate::DOCUMENT_SCRIPTS_DIR)
            .join("render_pdf.py");
        std::fs::create_dir_all(helper.parent().unwrap()).unwrap();
        std::fs::write(&helper, b"print('render')").unwrap();
        let fake = FakeWorkspace::default();

        let pushed = push_host_dir(&fake, &workspace_id(), host.path())
            .await
            .unwrap();

        assert_eq!(pushed.transferred, 1);
        assert!(pushed.notes.is_empty());
        assert_eq!(
            fake.get(".openwave/exec-scripts/render_pdf.py").unwrap(),
            b"print('render')"
        );
    }
}
