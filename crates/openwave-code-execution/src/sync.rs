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

use crate::host_paths::{try_resolve_scratch_directory, ScratchDir, ScratchRefusal};
use crate::{
    CodeExecutionError, ExecutionWorkspaceId, WorkspaceFilePath, WorkspaceLifecycle,
    MAX_WORKSPACE_FILE_BYTES,
};

/// The most files one direction of a sync will transfer before stopping and
/// saying so. A workspace that grows a dependency tree past the skip list
/// should degrade into a bounded transfer plus a note, not an unbounded copy.
pub const MAX_SYNC_FILES: usize = 256;

/// The most notes one direction of a sync will carry before collapsing
/// repeats into a summary line. Notes are rendered into the model-facing tool
/// result, so a tree full of skipped entries must not push unbounded prose
/// into the model's context: the first note of each distinct reason is always
/// kept, and anything past the limit with an already-shown reason becomes a
/// count.
pub const MAX_SYNC_NOTES: usize = 32;

/// Directory names never mirrored in either direction: version control and
/// dependency trees that are large, regenerable, and meaningless to copy
/// between the host and a sandbox.
const SKIPPED_DIRS: &[&str] = &[".git", ".venv", "__pycache__", "node_modules"];

/// What one direction of a sync transferred, and everything it left behind.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncReport {
    pub transferred: usize,
    pub notes: Vec<String>,
    /// Distinct note reasons already shown, in first-seen order.
    note_reasons: Vec<&'static str>,
    /// Notes collapsed into the summary line [`SyncReport::finish`] appends.
    notes_overflow: usize,
}

impl SyncReport {
    /// Record one skipped entry, bounding the report for the model: the first
    /// note of each distinct `reason` is always kept, while notes past
    /// [`MAX_SYNC_NOTES`] with an already-shown reason collapse into the
    /// overflow count [`SyncReport::finish`] summarizes.
    fn skip(&mut self, reason: &'static str, note: impl Into<String>) {
        if self.notes.len() >= MAX_SYNC_NOTES && self.note_reasons.contains(&reason) {
            self.notes_overflow += 1;
            return;
        }
        if !self.note_reasons.contains(&reason) {
            self.note_reasons.push(reason);
        }
        self.notes.push(note.into());
    }

    /// Append the overflow summary, if any, and hand the report back.
    fn finish(mut self, verb: &str) -> Self {
        if self.notes_overflow > 0 {
            self.notes.push(format!(
                "not {verb}: {} more note(s) beyond the {MAX_SYNC_NOTES}-note sync limit",
                self.notes_overflow
            ));
        }
        self
    }
}

/// Push every eligible file under `host_dir` into the workspace.
///
/// A missing `host_dir` pushes nothing: the chat simply has no scratch yet.
///
/// Every host-side failure — an unreadable file, a directory the walk cannot
/// list — is reported against the entry it belongs to and the push continues.
/// One permission-denied file in an attached folder should cost the agent that
/// file, not the whole workspace.
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
        let listed = if prefix.is_empty() {
            "the private scratch root".to_owned()
        } else {
            format!("{prefix}/")
        };
        let mut entries = match tokio::fs::read_dir(&dir).await {
            Ok(entries) => entries,
            Err(error) => {
                tracing::warn!("private scratch directory '{listed}' could not be listed: {error}");
                report.skip(
                    "could not be listed",
                    format!("not pushed: {listed} could not be listed ({error})"),
                );
                continue;
            }
        };
        loop {
            let entry = match entries.next_entry().await {
                Ok(Some(entry)) => entry,
                Ok(None) => break,
                Err(error) => {
                    tracing::warn!("listing of private scratch '{listed}' ended early: {error}");
                    report.skip(
                        "listing ended early",
                        format!("not pushed: the listing of {listed} ended early ({error})"),
                    );
                    break;
                }
            };
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                report.skip(
                    "non-UTF-8 name",
                    format!("not pushed: an entry under '{prefix}/' has a non-UTF-8 name"),
                );
                continue;
            };
            let relative = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let metadata = match tokio::fs::symlink_metadata(entry.path()).await {
                Ok(metadata) => metadata,
                Err(error) => {
                    tracing::debug!(
                        "private scratch entry '{relative}' vanished or is unreadable: {error}"
                    );
                    report.skip(
                        "could not be inspected",
                        format!("not pushed: {relative} could not be inspected ({error})"),
                    );
                    continue;
                }
            };
            if metadata.is_symlink() {
                report.skip(
                    "is a symlink",
                    format!("not pushed: {relative} is a symlink"),
                );
                continue;
            }
            if metadata.is_dir() {
                if SKIPPED_DIRS.contains(&name.as_str()) {
                    report.skip(
                        "dependency or VCS tree",
                        format!("not pushed: {relative}/ (dependency or VCS tree)"),
                    );
                } else {
                    stack.push((entry.path(), relative));
                }
                continue;
            }
            if !metadata.is_file() {
                continue;
            }
            if metadata.len() > MAX_WORKSPACE_FILE_BYTES as u64 {
                report.skip(
                    "exceeds the file limit",
                    format!(
                        "not pushed: {relative} exceeds the {MAX_WORKSPACE_FILE_BYTES}-byte file limit"
                    ),
                );
                continue;
            }
            let Ok(path) = WorkspaceFilePath::parse(&relative) else {
                report.skip(
                    "not a valid workspace path",
                    format!("not pushed: {relative} is not a valid workspace path"),
                );
                continue;
            };
            files.push((path, entry.path()));
        }
    }
    files.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
    let overflow = files.len().saturating_sub(MAX_SYNC_FILES);
    if overflow > 0 {
        report.skip(
            "beyond the file sync limit",
            format!(
                "not pushed: {overflow} more file(s) beyond the {MAX_SYNC_FILES}-file sync limit"
            ),
        );
    }
    for (path, host_path) in files.into_iter().take(MAX_SYNC_FILES) {
        let content = match tokio::fs::read(&host_path).await {
            Ok(content) => content,
            Err(error) => {
                tracing::debug!(
                    "private scratch file '{}' could not be read: {error}",
                    path.as_str()
                );
                report.skip(
                    "could not be read from the host",
                    format!(
                        "not pushed: {} could not be read from the host ({error})",
                        path.as_str()
                    ),
                );
                continue;
            }
        };
        match lifecycle
            .put_workspace_file(workspace, &path, &content)
            .await
        {
            Ok(()) => report.transferred += 1,
            Err(error) if aborts_the_sync(&error) => return Err(error),
            Err(error) => {
                tracing::warn!(
                    "private scratch file '{}' was rejected by the workspace: {error}",
                    path.as_str()
                );
                report.skip(
                    "rejected by the workspace",
                    format!(
                        "not pushed: {} was rejected by the workspace ({error})",
                        path.as_str()
                    ),
                );
            }
        }
    }
    Ok(report.finish("pushed"))
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
            report.skip(
                "listing was truncated",
                format!("not fully pulled: the listing of {shown} was truncated"),
            );
        }
        for entry in listing.entries {
            // The path comes back from the sandbox; re-validate it so a
            // hostile or confused backend cannot steer a write outside the
            // chat's scratch directory.
            let Ok(path) = WorkspaceFilePath::parse(&entry.path) else {
                report.skip(
                    "not a valid workspace path",
                    format!("not pulled: '{}' is not a valid workspace path", entry.path),
                );
                continue;
            };
            if entry.directory {
                if SKIPPED_DIRS.contains(&path.file_name()) {
                    report.skip(
                        "dependency or VCS tree",
                        format!("not pulled: {}/ (dependency or VCS tree)", path.as_str()),
                    );
                } else {
                    stack.push(Some(path));
                }
                continue;
            }
            if entry
                .size_bytes
                .is_some_and(|size| size > MAX_WORKSPACE_FILE_BYTES as u64)
            {
                report.skip(
                    "exceeds the file limit",
                    format!(
                        "not pulled: {} exceeds the {MAX_WORKSPACE_FILE_BYTES}-byte file limit",
                        path.as_str()
                    ),
                );
                continue;
            }
            files.push(path);
        }
    }
    files.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    let overflow = files.len().saturating_sub(MAX_SYNC_FILES);
    if overflow > 0 {
        report.skip(
            "beyond the file sync limit",
            format!(
                "not pulled: {overflow} more file(s) beyond the {MAX_SYNC_FILES}-file sync limit"
            ),
        );
    }
    let files: Vec<WorkspaceFilePath> = files.into_iter().take(MAX_SYNC_FILES).collect();
    if files.is_empty() {
        return Ok(report.finish("pulled"));
    }
    tokio::fs::create_dir_all(host_dir)
        .await
        .map_err(|_| unwritable_dir())?;
    for path in files {
        let content = match lifecycle.get_workspace_file(workspace, &path).await {
            Ok(content) => content,
            Err(error) if aborts_the_sync(&error) => return Err(error),
            Err(error) => {
                tracing::warn!(
                    "workspace file '{}' could not be pulled: {error}",
                    path.as_str()
                );
                report.skip(
                    "could not be read from the workspace",
                    format!(
                        "not pulled: {} could not be read from the workspace ({error})",
                        path.as_str()
                    ),
                );
                continue;
            }
        };
        let parent = match host_parent_dir(host_dir, &path).await {
            Ok(parent) => parent,
            Err(refusal) => {
                refuse(&mut report, &path, refusal);
                continue;
            }
        };
        // Everything below addresses `name` inside the pinned parent, so the
        // entry judged here is the entry written; nothing in between re-walks
        // the scratch tree from `/`.
        let name = path.file_name();
        // Read for the equality shortcut only after the symlink check: a
        // planted link should be reported as one rather than becoming a
        // content-equality oracle on whatever it points at.
        if parent.is_symlink(name).await {
            tracing::warn!(
                "workspace pull refused: '{}' is a symlink on the host",
                path.as_str()
            );
            report.skip(
                "is a symlink on the host",
                format!("not pulled: {} is a symlink on the host", path.as_str()),
            );
            continue;
        }
        if let Ok(existing) = parent.read_file(name).await {
            if existing == content {
                continue;
            }
        }
        parent
            .write_file(name, &content)
            .await
            .map_err(|_| unwritable(path.as_str()))?;
        report.transferred += 1;
    }
    Ok(report.finish("pulled"))
}

/// The pinned directory `path`'s file lands in under `host_dir`, creating
/// missing parents only through components already proven to be real
/// directories.
///
/// Local exec is confined to this same scratch tree, so it can plant
/// `<scratch>/out -> ~/.ssh` and wait for a pull: `create_dir_all` and a plain
/// `write` both follow a symlinked *parent*, and the pull's host write is not
/// sandboxed. Handing back a descriptor rather than a path also closes the
/// window between the walk and the write, which a process still running in
/// scratch could otherwise use to swap the verified directory for a symlink.
async fn host_parent_dir(
    host_dir: &Path,
    path: &WorkspaceFilePath,
) -> Result<ScratchDir, ScratchRefusal> {
    let parent = path
        .as_str()
        .rsplit_once('/')
        .map_or("", |(parent, _)| parent);
    try_resolve_scratch_directory(host_dir, parent, true).await
}

/// Record a refused file in the report and, for the containment cases, on the
/// host.
///
/// The note goes to the model; without the log line a sandbox probing the
/// boundary would generate output only it can see.
fn refuse(report: &mut SyncReport, path: &WorkspaceFilePath, refusal: ScratchRefusal) {
    let reason = match refusal {
        ScratchRefusal::Escape => "resolves outside the private scratch directory",
        ScratchRefusal::SymlinkedComponent => "has a symlinked parent directory on the host",
        ScratchRefusal::NotADirectory => "has a host parent that is not a directory",
        ScratchRefusal::Unavailable => "has a host parent directory that could not be created",
    };
    if refusal.is_containment() {
        tracing::warn!("workspace pull refused: '{}' {reason}", path.as_str());
    } else {
        tracing::debug!(
            "workspace file '{}' was not pulled: {reason}",
            path.as_str()
        );
    }
    report.skip(reason, format!("not pulled: {} {reason}", path.as_str()));
}

/// Whether one file's provider-side failure should end the whole sync.
///
/// A file that is missing, oversized, or rejected on its own merits is this
/// file's problem: the sync reports it and keeps going, which is what the
/// module promises. A provider that is unreachable or misconfigured is every
/// remaining file's problem too, and degrading it into a few hundred identical
/// notes would hide a real failure from the caller.
///
/// Only provider-side errors reach here. The push's other failures are
/// host-side — an unreadable file, a directory that vanished mid-walk — and
/// those are per-entry by nature: the next entry has every chance of
/// succeeding, so none of them are ever fatal.
fn aborts_the_sync(error: &CodeExecutionError) -> bool {
    match error {
        CodeExecutionError::WorkspaceFileNotFound
        | CodeExecutionError::WorkspaceFileTooLarge
        | CodeExecutionError::InvalidRequest(_)
        | CodeExecutionError::Sandbox(_) => false,
        CodeExecutionError::NotConfigured
        | CodeExecutionError::Unavailable(_)
        | CodeExecutionError::Spawn
        | CodeExecutionError::IdentityConflict
        | CodeExecutionError::AmbiguousExecution => true,
    }
}

fn unwritable(path: &str) -> CodeExecutionError {
    CodeExecutionError::Sandbox(format!("private scratch entry '{path}' is unwritable"))
}

fn unwritable_dir() -> CodeExecutionError {
    CodeExecutionError::Sandbox("the private scratch directory is unwritable".into())
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

    #[cfg(unix)]
    #[tokio::test]
    async fn pull_does_not_write_through_a_symlinked_parent_directory() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("authorized_keys"), b"host secret").unwrap();
        let host = tempfile::tempdir().unwrap();
        // Local exec is confined to the scratch directory but can create
        // entries in it, including a symlink aimed at a host directory.
        std::os::unix::fs::symlink(outside.path(), host.path().join("out")).unwrap();

        let fake = FakeWorkspace::default();
        fake.insert("out/authorized_keys", b"attacker key");

        let pulled = pull_into_host_dir(&fake, &workspace_id(), host.path())
            .await
            .unwrap();

        assert_eq!(pulled.transferred, 0, "{:?}", pulled.notes);
        assert_eq!(
            std::fs::read(outside.path().join("authorized_keys")).unwrap(),
            b"host secret"
        );
        assert!(pulled.notes.iter().any(|note| {
            note.contains("out/authorized_keys") && note.contains("symlinked parent directory")
        }));
    }

    #[tokio::test]
    async fn pull_reports_an_unreadable_file_and_keeps_going() {
        let host = tempfile::tempdir().unwrap();
        let fake = FakeWorkspace::default();
        fake.insert("kept.txt", b"sandbox output");
        // A backend that omits the size passes the oversize filter and can
        // still refuse the download; older E2B envd and Daytona both do.
        fake.planted.lock().unwrap().push(WorkspaceFileEntry {
            path: "huge.bin".into(),
            directory: false,
            size_bytes: None,
        });

        let pulled = pull_into_host_dir(&fake, &workspace_id(), host.path())
            .await
            .unwrap();

        assert_eq!(pulled.transferred, 1, "{:?}", pulled.notes);
        assert_eq!(
            std::fs::read(host.path().join("kept.txt")).unwrap(),
            b"sandbox output"
        );
        assert!(pulled
            .notes
            .iter()
            .any(|note| note.contains("huge.bin") && note.contains("could not be read")));
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

    #[cfg(unix)]
    #[tokio::test]
    async fn push_reports_an_unreadable_file_and_keeps_going() {
        use std::os::unix::fs::PermissionsExt;

        let host = tempfile::tempdir().unwrap();
        std::fs::write(host.path().join("readable.txt"), b"attached brief").unwrap();
        let denied = host.path().join("denied.txt");
        std::fs::write(&denied, b"secret").unwrap();
        std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o000)).unwrap();
        let fake = FakeWorkspace::default();

        let pushed = push_host_dir(&fake, &workspace_id(), host.path())
            .await
            .unwrap();

        assert_eq!(pushed.transferred, 1, "{:?}", pushed.notes);
        assert_eq!(fake.get("readable.txt").unwrap(), b"attached brief");
        assert!(fake.get("denied.txt").is_none());
        assert!(pushed
            .notes
            .iter()
            .any(|note| note.contains("denied.txt") && note.contains("could not be read")));
    }

    #[tokio::test]
    async fn pull_caps_sync_notes_and_preserves_distinct_reasons() {
        let host = tempfile::tempdir().unwrap();
        let fake = FakeWorkspace::default();
        fake.insert("kept.txt", b"sandbox output");
        // A hostile listing can produce a skip note per entry; the report must
        // stay bounded without losing a reason that only appears late.
        for i in 0..40 {
            fake.planted.lock().unwrap().push(WorkspaceFileEntry {
                path: format!("../evil-{i}.txt"),
                directory: false,
                size_bytes: Some(4),
            });
        }
        for i in 0..2 {
            fake.planted.lock().unwrap().push(WorkspaceFileEntry {
                path: format!("big-{i}.bin"),
                directory: false,
                size_bytes: Some(MAX_WORKSPACE_FILE_BYTES as u64 + 1),
            });
        }

        let pulled = pull_into_host_dir(&fake, &workspace_id(), host.path())
            .await
            .unwrap();

        assert_eq!(pulled.transferred, 1, "{:?}", pulled.notes);
        assert!(pulled.notes.len() < 42, "{:?}", pulled.notes);
        assert!(pulled.notes[0].contains("../evil-0.txt"));
        assert!(pulled
            .notes
            .iter()
            .any(|note| note.contains("big-0.bin") && note.contains("file limit")));
        assert_eq!(
            pulled.notes.last().unwrap(),
            &format!("not pulled: 9 more note(s) beyond the {MAX_SYNC_NOTES}-note sync limit")
        );
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
