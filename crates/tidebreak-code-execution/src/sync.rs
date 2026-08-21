//! Explicit file staging between host scratch and a managed workspace.
//!
//! Remote sandboxes have their own filesystem, but the model is shown one
//! path vocabulary: the file tools and `exec` both speak private-scratch
//! relative paths. The host keeps that story true by staging exactly the
//! paths the model listed on the call into the workspace before the command
//! runs, and by pulling only the `output/` and `preview/` subtrees back
//! afterwards — the two directories whose contents feed the host-side output
//! and preview scans.
//!
//! Staging is loud, never silent: a listed path that does not exist, is a
//! symlink, or expands past the per-call file bound fails the call with an
//! error naming the path. Only per-entry conditions discovered *inside* a
//! listed directory (a dependency tree, a nested symlink, an oversized file)
//! degrade into notes in the [`SyncReport`], because the entry beside them can
//! still be staged usefully.

use std::collections::HashSet;
use std::path::Path;

use crate::host_paths::{
    try_resolve_scratch_directory, ScratchDir, ScratchEntryKind, ScratchRefusal,
};
use crate::{
    ExecError, ExecutionWorkspaceId, StagedUpload, WorkspaceFilePath, WorkspaceLifecycle,
    MAX_WORKSPACE_FILE_BYTES,
};

/// The most files one call's listed paths may expand to. Crossing it fails the
/// call loudly rather than truncating the staged set: a bounded set the model
/// chose is useful, a silently incomplete one produces baffling not-found
/// errors inside the sandbox.
pub const MAX_STAGED_FILES: usize = 256;

/// The workspace subtrees pulled back to host scratch after a command.
pub const PULLED_DIRS: &[&str] = &["output", "preview"];

/// The most notes one sync direction will carry before collapsing repeats
/// into a summary line. Notes are rendered into the model-facing tool result,
/// so a tree full of skipped entries must not push unbounded prose into the
/// model's context: the first note of each distinct reason is always kept, and
/// anything past the limit with an already-shown reason becomes a count.
pub const MAX_SYNC_NOTES: usize = 32;

/// Directory names never staged or pulled: version control and dependency
/// trees that are large, regenerable, and meaningless to copy between the host
/// and a sandbox.
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

/// One host file a staging pass would upload, pinned to the directory it was
/// judged in so the entry read is the entry that was checked.
struct StagedFile {
    path: WorkspaceFilePath,
    dir: ScratchDir,
    name: String,
}

/// Validate the listed paths without transferring anything: every path must
/// exist under `host_dir`, none may be or cross a symlink, and the expansion
/// must stay within [`MAX_STAGED_FILES`].
///
/// The local provider calls this so a bad `files` entry fails identically on
/// every provider, even though scratch is already its filesystem.
pub async fn validate_staged_paths(
    host_dir: &Path,
    listed: &[WorkspaceFilePath],
) -> Result<(), ExecError> {
    resolve_staged_files(host_dir, listed).await.map(|_| ())
}

/// Stage exactly the listed paths into the workspace, expanding directories
/// recursively. Nothing outside the listed set is transferred.
///
/// Unchanged files may be skipped by a backend that remembers its live
/// session's staged content; the report counts only real transfers.
pub async fn stage_listed_paths(
    lifecycle: &dyn WorkspaceLifecycle,
    workspace: &ExecutionWorkspaceId,
    host_dir: &Path,
    listed: &[WorkspaceFilePath],
) -> Result<SyncReport, ExecError> {
    let (files, mut report) = resolve_staged_files(host_dir, listed).await?;
    for staged in files {
        let content = match staged.dir.read_file(&staged.name).await {
            Ok(content) => content,
            Err(error) => {
                tracing::debug!(
                    "staged file '{}' could not be read: {error}",
                    staged.path.as_str()
                );
                report.skip(
                    "could not be read from the host",
                    format!(
                        "not staged: {} could not be read from the host ({error})",
                        staged.path.as_str()
                    ),
                );
                continue;
            }
        };
        match lifecycle
            .stage_workspace_file(workspace, &staged.path, &content)
            .await
        {
            Ok(StagedUpload::Uploaded) => report.transferred += 1,
            Ok(StagedUpload::AlreadyCurrent) => {}
            Err(error) if aborts_the_sync(&error) => return Err(error),
            Err(error) => {
                tracing::warn!(
                    "staged file '{}' was rejected by the workspace: {error}",
                    staged.path.as_str()
                );
                report.skip(
                    "rejected by the workspace",
                    format!(
                        "not staged: {} was rejected by the workspace ({error})",
                        staged.path.as_str()
                    ),
                );
            }
        }
    }
    Ok(report.finish("staged"))
}

/// Resolve the listed paths into the concrete files a staging would upload.
///
/// Failures on the listed paths themselves are errors that name the path;
/// conditions inside an expanded directory become notes.
async fn resolve_staged_files(
    host_dir: &Path,
    listed: &[WorkspaceFilePath],
) -> Result<(Vec<StagedFile>, SyncReport), ExecError> {
    let mut report = SyncReport::default();
    let mut files: Vec<StagedFile> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for path in listed {
        let (parent_rel, name) = path
            .as_str()
            .rsplit_once('/')
            .map_or(("", path.as_str()), |(parent, name)| (parent, name));
        let parent = try_resolve_scratch_directory(host_dir, parent_rel, false)
            .await
            .map_err(|refusal| listed_path_error(path, refusal))?;
        if parent.is_symlink(name).await {
            return Err(ExecError::InvalidRequest(format!(
                "staged path '{}' is a symlink; symlinks are never staged",
                path.as_str()
            )));
        }
        if let Ok(dir) = parent.open_dir(name).await {
            expand_directory(dir, path, &mut files, &mut seen, &mut report).await?;
        } else {
            let Some(stamp) = parent.file_stamp(name).await else {
                return Err(ExecError::InvalidRequest(format!(
                    "staged path '{}' does not exist in the chat's files",
                    path.as_str()
                )));
            };
            if stamp.len > MAX_WORKSPACE_FILE_BYTES as u64 {
                return Err(ExecError::InvalidRequest(format!(
                    "staged path '{}' exceeds the {MAX_WORKSPACE_FILE_BYTES}-byte file limit",
                    path.as_str()
                )));
            }
            if seen.insert(path.as_str().to_owned()) {
                files.push(StagedFile {
                    path: path.clone(),
                    dir: parent,
                    name: name.to_owned(),
                });
            }
        }
        if files.len() > MAX_STAGED_FILES {
            return Err(staging_bound_error(path, files.len()));
        }
    }
    Ok((files, report))
}

/// Walk one listed directory, collecting its stageable files.
///
/// Crossing [`MAX_STAGED_FILES`] mid-walk fails immediately — continuing would
/// only enumerate a tree the call has already refused to transfer.
async fn expand_directory(
    root: ScratchDir,
    listed: &WorkspaceFilePath,
    files: &mut Vec<StagedFile>,
    seen: &mut HashSet<String>,
    report: &mut SyncReport,
) -> Result<(), ExecError> {
    let mut stack: Vec<(ScratchDir, String)> = vec![(root, listed.as_str().to_owned())];
    while let Some((dir, prefix)) = stack.pop() {
        let entries = match dir.entries().await {
            Ok(entries) => entries,
            Err(error) => {
                tracing::warn!("staged directory '{prefix}/' could not be listed: {error}");
                report.skip(
                    "could not be listed",
                    format!("not staged: {prefix}/ could not be listed ({error})"),
                );
                continue;
            }
        };
        for entry in entries {
            let name = entry.name;
            let relative = format!("{prefix}/{name}");
            match entry.kind {
                ScratchEntryKind::Directory => {
                    if SKIPPED_DIRS.contains(&name.as_str()) {
                        report.skip(
                            "dependency or VCS tree",
                            format!("not staged: {relative}/ (dependency or VCS tree)"),
                        );
                        continue;
                    }
                    // Open the child while it is being judged: the no-follow
                    // open refuses a directory swapped for a symlink since the
                    // listing instead of walking into wherever it points.
                    match dir.open_dir(&name).await {
                        Ok(child) => stack.push((child, relative)),
                        Err(error) => {
                            tracing::warn!(
                                "staged directory '{relative}/' could not be entered: {error}"
                            );
                            report.skip(
                                "could not be listed",
                                format!("not staged: {relative}/ could not be listed ({error})"),
                            );
                        }
                    }
                }
                ScratchEntryKind::File => {
                    let Some(stamp) = dir.file_stamp(&name).await else {
                        report.skip(
                            "could not be inspected",
                            format!("not staged: {relative} could not be inspected"),
                        );
                        continue;
                    };
                    if stamp.len > MAX_WORKSPACE_FILE_BYTES as u64 {
                        report.skip(
                            "exceeds the file limit",
                            format!(
                                "not staged: {relative} exceeds the {MAX_WORKSPACE_FILE_BYTES}-byte file limit"
                            ),
                        );
                        continue;
                    }
                    let Ok(path) = WorkspaceFilePath::parse(&relative) else {
                        report.skip(
                            "not a valid workspace path",
                            format!("not staged: {relative} is not a valid workspace path"),
                        );
                        continue;
                    };
                    if seen.insert(path.as_str().to_owned()) {
                        files.push(StagedFile {
                            path,
                            dir: dir.clone(),
                            name,
                        });
                    }
                    if files.len() > MAX_STAGED_FILES {
                        return Err(staging_bound_error(listed, files.len()));
                    }
                }
                // The symlink question only decides which note to write;
                // nothing here traverses the entry either way.
                ScratchEntryKind::Other => {
                    if dir.is_symlink(&name).await {
                        report.skip(
                            "is a symlink",
                            format!("not staged: {relative} is a symlink"),
                        );
                    }
                }
            }
        }
    }
    Ok(())
}

fn staging_bound_error(listed: &WorkspaceFilePath, count: usize) -> ExecError {
    ExecError::InvalidRequest(format!(
        "staging '{}' expands the staged set past the {MAX_STAGED_FILES}-file bound \
         ({count}+ files); list fewer or more specific paths",
        listed.as_str()
    ))
}

fn listed_path_error(path: &WorkspaceFilePath, refusal: ScratchRefusal) -> ExecError {
    let reason = match refusal {
        ScratchRefusal::Escape => "escapes the chat's files",
        ScratchRefusal::SymlinkedComponent => "crosses a symlink",
        ScratchRefusal::NotADirectory | ScratchRefusal::Unavailable => {
            "does not exist in the chat's files"
        }
    };
    ExecError::InvalidRequest(format!("staged path '{}' {reason}", path.as_str()))
}

/// Pull the `output/` and `preview/` subtrees back into `host_dir`, writing
/// only files whose content actually differs from the host copy. Nothing
/// outside those two directories is read back: intermediates the command
/// created stay in the sandbox for later commands in the same session.
pub async fn pull_result_dirs(
    lifecycle: &dyn WorkspaceLifecycle,
    workspace: &ExecutionWorkspaceId,
    host_dir: &Path,
) -> Result<SyncReport, ExecError> {
    let mut report = SyncReport::default();
    let mut stack: Vec<WorkspaceFilePath> = PULLED_DIRS
        .iter()
        .filter_map(|root| WorkspaceFilePath::parse(*root).ok())
        .collect();
    let mut files: Vec<WorkspaceFilePath> = Vec::new();
    while let Some(dir) = stack.pop() {
        let listing = match lifecycle.list_workspace_files(workspace, Some(&dir)).await {
            Ok(listing) => listing,
            // A workspace with no output/ or preview/ yet has nothing to pull;
            // a directory that vanished mid-walk means the same.
            Err(ExecError::WorkspaceFileNotFound) => continue,
            Err(error) => return Err(error),
        };
        if listing.truncated {
            report.skip(
                "listing was truncated",
                format!(
                    "not fully pulled: the listing of {} was truncated",
                    dir.as_str()
                ),
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
            // Scope, not just containment: only output/ and preview/ come
            // back, even if the backend lists something else.
            if !PULLED_DIRS.iter().any(|root| {
                path.as_str()
                    .strip_prefix(root)
                    .is_some_and(|rest| rest.starts_with('/'))
            }) {
                report.skip(
                    "outside the pulled directories",
                    format!(
                        "not pulled: {} is outside output/ and preview/",
                        path.as_str()
                    ),
                );
                continue;
            }
            if entry.directory {
                if SKIPPED_DIRS.contains(&path.file_name()) {
                    report.skip(
                        "dependency or VCS tree",
                        format!("not pulled: {}/ (dependency or VCS tree)", path.as_str()),
                    );
                } else {
                    stack.push(path);
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
    let overflow = files.len().saturating_sub(MAX_STAGED_FILES);
    if overflow > 0 {
        report.skip(
            "beyond the file sync limit",
            format!(
                "not pulled: {overflow} more file(s) beyond the {MAX_STAGED_FILES}-file sync limit"
            ),
        );
    }
    let files: Vec<WorkspaceFilePath> = files.into_iter().take(MAX_STAGED_FILES).collect();
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
/// `<scratch>/output -> ~/.ssh` and wait for a pull: `create_dir_all` and a
/// plain `write` both follow a symlinked *parent*, and the pull's host write
/// is not sandboxed. Handing back a descriptor rather than a path also closes
/// the window between the walk and the write, which a process still running in
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
/// file's problem: the sync reports it and keeps going. A provider that is
/// unreachable or misconfigured is every remaining file's problem too, and
/// degrading it into a few hundred identical notes would hide a real failure
/// from the caller.
///
/// Only provider-side errors reach here. The staging's other failures are
/// host-side — an unreadable file, a directory that vanished mid-walk — and
/// those are per-entry by nature: the next entry has every chance of
/// succeeding, so none of them are ever fatal.
fn aborts_the_sync(error: &ExecError) -> bool {
    match error {
        ExecError::WorkspaceFileNotFound
        | ExecError::WorkspaceFileTooLarge
        | ExecError::InvalidRequest(_)
        | ExecError::Sandbox(_) => false,
        ExecError::NotConfigured
        | ExecError::Unavailable(_)
        | ExecError::Spawn
        | ExecError::IdentityConflict
        | ExecError::AmbiguousExecution => true,
    }
}

fn unwritable(path: &str) -> ExecError {
    ExecError::Sandbox(format!("private scratch entry '{path}' is unwritable"))
}

fn unwritable_dir() -> ExecError {
    ExecError::Sandbox("the private scratch directory is unwritable".into())
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
        /// Extra rows returned verbatim from the `output/` listing, for
        /// hostile-backend cases.
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
        ) -> Result<(), ExecError> {
            Ok(())
        }

        async fn connect_workspace(
            &self,
            _workspace: &ExecutionWorkspaceId,
        ) -> Result<bool, ExecError> {
            Ok(true)
        }

        async fn destroy_workspace(
            &self,
            _workspace: &ExecutionWorkspaceId,
        ) -> Result<(), ExecError> {
            Ok(())
        }

        async fn put_workspace_file(
            &self,
            _workspace: &ExecutionWorkspaceId,
            path: &WorkspaceFilePath,
            content: &[u8],
        ) -> Result<(), ExecError> {
            self.insert(path.as_str(), content);
            Ok(())
        }

        async fn get_workspace_file(
            &self,
            _workspace: &ExecutionWorkspaceId,
            path: &WorkspaceFilePath,
        ) -> Result<Vec<u8>, ExecError> {
            self.get(path.as_str())
                .ok_or_else(|| ExecError::Sandbox("missing file".into()))
        }

        async fn list_workspace_files(
            &self,
            _workspace: &ExecutionWorkspaceId,
            path: Option<&WorkspaceFilePath>,
        ) -> Result<WorkspaceListing, ExecError> {
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
            if path.map(WorkspaceFilePath::as_str) == Some("output") {
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

    fn paths(listed: &[&str]) -> Vec<WorkspaceFilePath> {
        listed
            .iter()
            .map(|path| WorkspaceFilePath::parse(*path).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn stages_exactly_the_listed_paths_and_expands_directories() {
        let host = tempfile::tempdir().unwrap();
        std::fs::write(host.path().join("a.txt"), "listed file").unwrap();
        std::fs::write(host.path().join("junk.txt"), "never listed").unwrap();
        std::fs::create_dir_all(host.path().join("sub/nested")).unwrap();
        std::fs::write(host.path().join("sub/b.txt"), "in listed dir").unwrap();
        std::fs::write(host.path().join("sub/nested/c.txt"), "nested").unwrap();
        std::fs::create_dir_all(host.path().join("sub/node_modules")).unwrap();
        std::fs::write(host.path().join("sub/node_modules/dep.js"), "never").unwrap();

        let fake = FakeWorkspace::default();
        let report = stage_listed_paths(
            &fake,
            &workspace_id(),
            host.path(),
            &paths(&["a.txt", "sub"]),
        )
        .await
        .unwrap();

        assert_eq!(report.transferred, 3, "{:?}", report.notes);
        assert_eq!(fake.get("a.txt").unwrap(), b"listed file");
        assert_eq!(fake.get("sub/b.txt").unwrap(), b"in listed dir");
        assert_eq!(fake.get("sub/nested/c.txt").unwrap(), b"nested");
        assert!(fake.get("junk.txt").is_none(), "unlisted files never move");
        assert!(fake.get("sub/node_modules/dep.js").is_none());
        assert_eq!(
            report.notes,
            vec!["not staged: sub/node_modules/ (dependency or VCS tree)"]
        );
    }

    #[tokio::test]
    async fn a_missing_listed_path_fails_naming_it() {
        let host = tempfile::tempdir().unwrap();
        std::fs::write(host.path().join("present.txt"), "here").unwrap();
        let fake = FakeWorkspace::default();

        let error = stage_listed_paths(
            &fake,
            &workspace_id(),
            host.path(),
            &paths(&["present.txt", "build_deck.py"]),
        )
        .await
        .unwrap_err();

        let message = error.to_string();
        assert!(message.contains("build_deck.py"), "{message}");
        assert!(message.contains("does not exist"), "{message}");
        // The same contract holds for the validation-only entry point the
        // local provider uses.
        let local = validate_staged_paths(host.path(), &paths(&["build_deck.py"]))
            .await
            .unwrap_err();
        assert!(local.to_string().contains("build_deck.py"));
    }

    #[tokio::test]
    async fn expansion_past_the_bound_fails_naming_the_listed_path() {
        let host = tempfile::tempdir().unwrap();
        let cache = host.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        for index in 0..=MAX_STAGED_FILES {
            std::fs::write(cache.join(format!("f{index:04}")), "x").unwrap();
        }
        let fake = FakeWorkspace::default();

        let error = stage_listed_paths(&fake, &workspace_id(), host.path(), &paths(&["cache"]))
            .await
            .unwrap_err();

        let message = error.to_string();
        assert!(message.contains("'cache'"), "{message}");
        assert!(message.contains(&MAX_STAGED_FILES.to_string()), "{message}");
        // Loud means loud: nothing was uploaded from a refused staging.
        assert!(fake.files.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_oversized_listed_file_fails_and_an_oversized_nested_file_is_noted() {
        let host = tempfile::tempdir().unwrap();
        let oversized = std::fs::File::create(host.path().join("big.bin")).unwrap();
        oversized
            .set_len(MAX_WORKSPACE_FILE_BYTES as u64 + 1)
            .unwrap();
        std::fs::create_dir_all(host.path().join("data")).unwrap();
        std::fs::write(host.path().join("data/ok.txt"), "fits").unwrap();
        let nested = std::fs::File::create(host.path().join("data/huge.bin")).unwrap();
        nested.set_len(MAX_WORKSPACE_FILE_BYTES as u64 + 1).unwrap();
        let fake = FakeWorkspace::default();

        let error = stage_listed_paths(&fake, &workspace_id(), host.path(), &paths(&["big.bin"]))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("big.bin"), "{error}");

        let report = stage_listed_paths(&fake, &workspace_id(), host.path(), &paths(&["data"]))
            .await
            .unwrap();
        assert_eq!(report.transferred, 1, "{:?}", report.notes);
        assert_eq!(fake.get("data/ok.txt").unwrap(), b"fits");
        assert!(fake.get("data/huge.bin").is_none());
        assert!(report
            .notes
            .iter()
            .any(|note| note.contains("data/huge.bin") && note.contains("file limit")));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_listed_symlink_fails_and_a_nested_symlink_is_noted() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), b"host secret").unwrap();
        let host = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret"), host.path().join("link"))
            .unwrap();
        std::fs::create_dir_all(host.path().join("data")).unwrap();
        std::fs::write(host.path().join("data/real.txt"), "real").unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret"), host.path().join("data/link"))
            .unwrap();
        let fake = FakeWorkspace::default();

        let error = stage_listed_paths(&fake, &workspace_id(), host.path(), &paths(&["link"]))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("'link'"), "{error}");
        assert!(error.to_string().contains("symlink"), "{error}");

        // A listed path that reaches through a symlinked directory fails too.
        std::os::unix::fs::symlink(outside.path(), host.path().join("dir-link")).unwrap();
        let error = stage_listed_paths(
            &fake,
            &workspace_id(),
            host.path(),
            &paths(&["dir-link/secret"]),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("dir-link/secret"), "{error}");

        let report = stage_listed_paths(&fake, &workspace_id(), host.path(), &paths(&["data"]))
            .await
            .unwrap();
        assert_eq!(report.transferred, 1, "{:?}", report.notes);
        assert!(fake.get("data/link").is_none());
        assert!(report
            .notes
            .iter()
            .any(|note| note.contains("data/link") && note.contains("is a symlink")));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn staging_notes_an_unreadable_nested_file_and_keeps_going() {
        use std::os::unix::fs::PermissionsExt;

        let host = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(host.path().join("docs")).unwrap();
        std::fs::write(host.path().join("docs/readable.txt"), b"attached brief").unwrap();
        let denied = host.path().join("docs/denied.txt");
        std::fs::write(&denied, b"secret").unwrap();
        std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o000)).unwrap();
        let fake = FakeWorkspace::default();

        let report = stage_listed_paths(&fake, &workspace_id(), host.path(), &paths(&["docs"]))
            .await
            .unwrap();

        assert_eq!(report.transferred, 1, "{:?}", report.notes);
        assert_eq!(fake.get("docs/readable.txt").unwrap(), b"attached brief");
        assert!(fake.get("docs/denied.txt").is_none());
        assert!(report
            .notes
            .iter()
            .any(|note| note.contains("denied.txt") && note.contains("could not be read")));
    }

    #[tokio::test]
    async fn pull_touches_only_output_and_preview() {
        let host = tempfile::tempdir().unwrap();
        let fake = FakeWorkspace::default();
        fake.insert("output/report.csv", b"rows");
        fake.insert("output/sub/extra.txt", b"nested output");
        fake.insert("preview/overview.png", b"pixels");
        fake.insert("scratch-root.txt", b"intermediate");
        fake.insert("work/data.bin", b"intermediate");

        let pulled = pull_result_dirs(&fake, &workspace_id(), host.path())
            .await
            .unwrap();

        assert_eq!(pulled.transferred, 3, "{:?}", pulled.notes);
        assert_eq!(
            std::fs::read(host.path().join("output/report.csv")).unwrap(),
            b"rows"
        );
        assert_eq!(
            std::fs::read(host.path().join("output/sub/extra.txt")).unwrap(),
            b"nested output"
        );
        assert_eq!(
            std::fs::read(host.path().join("preview/overview.png")).unwrap(),
            b"pixels"
        );
        assert!(!host.path().join("scratch-root.txt").exists());
        assert!(!host.path().join("work").exists());
        // Unchanged content is not rewritten on a second pull.
        let again = pull_result_dirs(&fake, &workspace_id(), host.path())
            .await
            .unwrap();
        assert_eq!(again.transferred, 0, "{:?}", again.notes);
    }

    #[tokio::test]
    async fn pull_rejects_listing_rows_that_escape_or_leave_the_pulled_scope() {
        let parent = tempfile::tempdir().unwrap();
        let host = parent.path().join("scratch");
        std::fs::create_dir_all(&host).unwrap();

        let fake = FakeWorkspace::default();
        fake.insert("secrets.txt", b"root file the backend should not offer");
        fake.planted.lock().unwrap().push(WorkspaceFileEntry {
            path: "../escape.txt".into(),
            directory: false,
            size_bytes: Some(4),
        });
        fake.planted.lock().unwrap().push(WorkspaceFileEntry {
            path: "secrets.txt".into(),
            directory: false,
            size_bytes: Some(4),
        });
        fake.planted.lock().unwrap().push(WorkspaceFileEntry {
            path: "output/big.bin".into(),
            directory: false,
            size_bytes: Some(MAX_WORKSPACE_FILE_BYTES as u64 + 1),
        });

        let pulled = pull_result_dirs(&fake, &workspace_id(), &host)
            .await
            .unwrap();
        assert_eq!(pulled.transferred, 0);
        assert!(!parent.path().join("escape.txt").exists());
        assert!(!host.join("secrets.txt").exists());
        assert_eq!(pulled.notes.len(), 3, "{:?}", pulled.notes);
        assert!(pulled.notes[0].contains("not a valid workspace path"));
        assert!(pulled.notes[1].contains("outside output/ and preview/"));
        assert!(pulled.notes[2].contains("file limit"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pull_does_not_write_through_a_symlinked_parent_directory() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("authorized_keys"), b"host secret").unwrap();
        let host = tempfile::tempdir().unwrap();
        // Local exec is confined to the scratch directory but can create
        // entries in it, including a symlink aimed at a host directory.
        std::os::unix::fs::symlink(outside.path(), host.path().join("output")).unwrap();

        let fake = FakeWorkspace::default();
        fake.insert("output/authorized_keys", b"attacker key");

        let pulled = pull_result_dirs(&fake, &workspace_id(), host.path())
            .await
            .unwrap();

        assert_eq!(pulled.transferred, 0, "{:?}", pulled.notes);
        assert_eq!(
            std::fs::read(outside.path().join("authorized_keys")).unwrap(),
            b"host secret"
        );
        assert!(pulled.notes.iter().any(|note| {
            note.contains("output/authorized_keys") && note.contains("symlinked parent directory")
        }));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pull_reports_a_symlink_planted_at_the_destination() {
        let outside = tempfile::tempdir().unwrap();
        // The target holds exactly what the sandbox reports, so a pull that
        // read through the link would judge the file up to date and say
        // nothing: the planted entry would never be surfaced.
        std::fs::write(outside.path().join("config"), b"same bytes").unwrap();
        let host = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(host.path().join("output")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("config"),
            host.path().join("output/config"),
        )
        .unwrap();

        let fake = FakeWorkspace::default();
        fake.insert("output/config", b"same bytes");

        let pulled = pull_result_dirs(&fake, &workspace_id(), host.path())
            .await
            .unwrap();

        assert_eq!(pulled.transferred, 0, "{:?}", pulled.notes);
        assert!(pulled.notes.iter().any(|note| {
            note.contains("output/config") && note.contains("is a symlink on the host")
        }));
        assert_eq!(
            std::fs::read(outside.path().join("config")).unwrap(),
            b"same bytes"
        );
        assert!(std::fs::symlink_metadata(host.path().join("output/config"))
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[tokio::test]
    async fn pull_reports_an_unreadable_file_and_keeps_going() {
        let host = tempfile::tempdir().unwrap();
        let fake = FakeWorkspace::default();
        fake.insert("output/kept.txt", b"sandbox output");
        // A backend that omits the size passes the oversize filter and can
        // still refuse the download; older E2B envd and Daytona both do.
        fake.planted.lock().unwrap().push(WorkspaceFileEntry {
            path: "output/huge.bin".into(),
            directory: false,
            size_bytes: None,
        });

        let pulled = pull_result_dirs(&fake, &workspace_id(), host.path())
            .await
            .unwrap();

        assert_eq!(pulled.transferred, 1, "{:?}", pulled.notes);
        assert_eq!(
            std::fs::read(host.path().join("output/kept.txt")).unwrap(),
            b"sandbox output"
        );
        assert!(pulled
            .notes
            .iter()
            .any(|note| note.contains("huge.bin") && note.contains("could not be read")));
    }

    #[tokio::test]
    async fn pull_caps_sync_notes_and_preserves_distinct_reasons() {
        let host = tempfile::tempdir().unwrap();
        let fake = FakeWorkspace::default();
        fake.insert("output/kept.txt", b"sandbox output");
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
                path: format!("output/big-{i}.bin"),
                directory: false,
                size_bytes: Some(MAX_WORKSPACE_FILE_BYTES as u64 + 1),
            });
        }

        let pulled = pull_result_dirs(&fake, &workspace_id(), host.path())
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
}
