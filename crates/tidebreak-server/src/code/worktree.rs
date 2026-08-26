//! Git worktree operations for code-mode workspaces.
//!
//! Every git call is a bounded, non-interactive subprocess of the user's own
//! `git` binary. Arguments are an argv array, never a shell string.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::time::timeout;

use super::setup_script::run_workspace_script;

const GIT_TIMEOUT: Duration = Duration::from_secs(30);
const GIT_WORKTREE_TIMEOUT: Duration = Duration::from_secs(120);
const SEARCH_TIMEOUT: Duration = Duration::from_secs(15);
/// Default number of paths the tree route returns.
pub(crate) const DEFAULT_TREE_LIMIT: u32 = 50;
/// Hard cap on the tree route. The explorer may request this many paths.
pub(crate) const MAX_TREE_LIMIT: u32 = 5_000;
/// Default number of matching lines returned by content search.
pub(crate) const DEFAULT_SEARCH_LIMIT: u32 = 200;
/// Hard cap for one content-search response.
pub(crate) const MAX_SEARCH_LIMIT: u32 = 500;
const MAX_SEARCH_QUERY_CHARS: usize = 500;
const MAX_SEARCH_PREVIEW_CHARS: usize = 500;
const ARCHIVE_SCAN_MAX_ENTRIES: usize = 10_000;
const ARCHIVE_SCAN_MAX_PATH_BYTES: usize = 1024 * 1024;
const ARCHIVE_DISPOSABLE_PATH_KEY: &str = "tidebreak.archiveDisposablePath";
const WORKTREE_OPERATION_SUFFIX: &str = ".tidebreak-operation";
const WORKTREE_REGISTRATION_MARKER: &str = "tidebreak-operation.json";

const ADJECTIVES: &[&str] = &[
    "amber", "brave", "calm", "crisp", "dusk", "ember", "faint", "gentle", "hidden", "ivory",
    "keen", "lunar", "misty", "noble", "open", "pale", "quiet", "rapid", "still", "vivid",
];
const NOUNS: &[&str] = &[
    "anchor", "brook", "cedar", "dune", "field", "grove", "harbor", "inlet", "ledge", "meadow",
    "notch", "orchard", "pine", "ridge", "shore", "thicket", "vale", "willow", "yard", "zenith",
];

/// A validated, canonical local git repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ValidatedRepo {
    pub toplevel: PathBuf,
}

/// Whether two canonical repository paths identify the same checkout.
///
/// Windows can present one path as a regular drive/UNC path or as the verbatim
/// form returned by `canonicalize`. Repository identity is also
/// case-insensitive there.
#[cfg(windows)]
pub(crate) fn repo_paths_equivalent(left: &Path, right: &Path) -> bool {
    use windows_sys::Win32::Globalization::{CompareStringOrdinal, CSTR_EQUAL};

    let left = windows_repo_path_identity(left);
    let right = windows_repo_path_identity(right);
    let (Ok(left_len), Ok(right_len)) = (i32::try_from(left.len()), i32::try_from(right.len()))
    else {
        return false;
    };
    // SAFETY: both pointers reference initialized UTF-16 buffers for the
    // exact lengths passed. CompareStringOrdinal does not require NUL
    // termination when explicit lengths are provided.
    unsafe {
        CompareStringOrdinal(left.as_ptr(), left_len, right.as_ptr(), right_len, 1) == CSTR_EQUAL
    }
}

#[cfg(not(windows))]
pub(crate) fn repo_paths_equivalent(left: &Path, right: &Path) -> bool {
    left == right
}

#[cfg(windows)]
fn windows_repo_path_identity(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;

    const VERBATIM: &[u16] = &[b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];
    const VERBATIM_UNC: &[u16] = &[
        b'\\' as u16,
        b'\\' as u16,
        b'?' as u16,
        b'\\' as u16,
        b'U' as u16,
        b'N' as u16,
        b'C' as u16,
        b'\\' as u16,
    ];

    let mut value = path
        .as_os_str()
        .encode_wide()
        .map(|unit| {
            if unit == b'/' as u16 {
                b'\\' as u16
            } else {
                unit
            }
        })
        .collect::<Vec<_>>();
    if wide_starts_with_ascii_case_insensitive(&value, VERBATIM_UNC) {
        value.splice(..VERBATIM_UNC.len(), [b'\\' as u16, b'\\' as u16]);
    } else if wide_starts_with_ascii_case_insensitive(&value, VERBATIM) {
        value.drain(..VERBATIM.len());
    }
    while value.last() == Some(&(b'\\' as u16)) {
        value.pop();
    }
    value
}

#[cfg(windows)]
fn wide_starts_with_ascii_case_insensitive(value: &[u16], prefix: &[u16]) -> bool {
    value.len() >= prefix.len()
        && value
            .iter()
            .zip(prefix)
            .all(|(left, right)| wide_ascii_upper(*left) == *right)
}

#[cfg(windows)]
fn wide_ascii_upper(unit: u16) -> u16 {
    if (b'a' as u16..=b'z' as u16).contains(&unit) {
        unit - (b'a' - b'A') as u16
    } else {
        unit
    }
}

/// Why a workspace archive needs an explicit `force`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ArchiveBlock {
    Uncommitted,
    Unpushed,
    UncommittedAndUnpushed,
    IgnoredContent,
}

impl ArchiveBlock {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Uncommitted => "uncommitted",
            Self::Unpushed => "unpushed",
            Self::UncommittedAndUnpushed => "uncommitted_and_unpushed",
            Self::IgnoredContent => "ignored_content",
        }
    }
}

/// Failure from a git or worktree operation.
#[derive(Debug, thiserror::Error)]
pub(crate) enum WorktreeError {
    #[error("{0}")]
    User(String),
    #[error("{0}")]
    Internal(String),
    #[error("{0}")]
    ArchiveUncertain(String),
    #[error("{message}")]
    Conflict { kind: &'static str, message: String },
}

impl WorktreeError {
    pub(crate) fn user(message: impl Into<String>) -> Self {
        Self::User(message.into())
    }

    fn internal(message: impl Into<String>) -> Self {
        Self::Internal(message.into())
    }

    fn archive_uncertain(message: impl Into<String>) -> Self {
        Self::ArchiveUncertain(message.into())
    }

    fn conflict(kind: &'static str, message: impl Into<String>) -> Self {
        Self::Conflict {
            kind,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct WorktreeOperationMarker {
    operation_id: uuid::Uuid,
    repository: String,
    worktree_path: String,
    branch: String,
    expected_tip: String,
}

/// A checkout operation that still owns the target reservation.
///
/// The caller keeps this value until the workspace's final lifecycle row is
/// durable. A failed durable write can then remove only the checkout and ref
/// that still match this operation.
#[derive(Debug)]
pub(crate) struct WorktreeOperation {
    repo_root: PathBuf,
    marker_path: PathBuf,
    marker: WorktreeOperationMarker,
    branch_created: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RestoredBranch {
    Created,
    Existing,
}

/// Name-matched git-tracked and untracked-unignored paths. Never file contents.
pub(crate) async fn list_tree_paths(
    worktree_path: &Path,
    query: &str,
    limit: u32,
) -> Result<(Vec<String>, bool), WorktreeError> {
    let limit = tree_limit(limit);
    let listed = git_nul_stdout(
        Some(worktree_path),
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
        GIT_TIMEOUT,
    )
    .await
    .map_err(|err| WorktreeError::internal(format!("git ls-files failed: {err}")))?;
    let needle = query.trim().to_ascii_lowercase();
    let mut matched = listed
        .into_iter()
        .filter(|path| !path.is_empty())
        .filter(|path| needle.is_empty() || path_name_matches(path, &needle))
        .collect::<Vec<_>>();
    matched.sort();
    matched.dedup();
    let truncated = matched.len() > limit;
    matched.truncate(limit);
    Ok((matched, truncated))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorktreeSearchMatch {
    pub(crate) path: String,
    pub(crate) line_number: u32,
    pub(crate) line: String,
}

/// Literal, case-insensitive content search across tracked and untracked files
/// that are not ignored by Git. Git's own grep engine keeps the hot path fast;
/// results are streamed and the process is stopped after one row beyond the
/// requested bound, so a one-character query cannot flood server memory.
pub(crate) async fn search_worktree_contents(
    worktree_path: &Path,
    query: &str,
    include: &str,
    exclude: &str,
    limit: u32,
) -> Result<(Vec<WorktreeSearchMatch>, bool), WorktreeError> {
    let query = query.trim();
    if query.is_empty() {
        return Ok((Vec::new(), false));
    }
    if query.chars().count() > MAX_SEARCH_QUERY_CHARS {
        return Err(WorktreeError::user(format!(
            "search query must be at most {MAX_SEARCH_QUERY_CHARS} characters"
        )));
    }

    let limit = search_limit(limit);
    let mut args = vec![
        "grep".to_owned(),
        "--no-index".to_owned(),
        "--exclude-standard".to_owned(),
        "--null".to_owned(),
        "--line-number".to_owned(),
        "-I".to_owned(),
        "--ignore-case".to_owned(),
        "--fixed-strings".to_owned(),
        "--no-color".to_owned(),
        "--full-name".to_owned(),
        "-e".to_owned(),
        query.to_owned(),
        "--".to_owned(),
    ];
    args.extend(search_pathspecs(include, exclude));

    let mut command = Command::new("git");
    command
        .args(&args)
        .current_dir(worktree_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env("GIT_TERMINAL_PROMPT", "0");
    let mut child = command
        .spawn()
        .map_err(|err| WorktreeError::internal(format!("failed to spawn git grep: {err}")))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| WorktreeError::internal("git grep stdout was not piped"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| WorktreeError::internal("git grep stderr was not piped"))?;

    let searched = timeout(SEARCH_TIMEOUT, async {
        let mut reader = BufReader::new(stdout);
        let mut matches = Vec::with_capacity(limit.min(64));
        let mut truncated = false;

        loop {
            let mut path = Vec::new();
            if reader.read_until(0, &mut path).await.map_err(|err| {
                WorktreeError::internal(format!("could not read git grep path: {err}"))
            })? == 0
            {
                break;
            }
            if path.last() == Some(&0) {
                path.pop();
            }

            let mut line_number = Vec::new();
            reader
                .read_until(0, &mut line_number)
                .await
                .map_err(|err| {
                    WorktreeError::internal(format!("could not read git grep line number: {err}"))
                })?;
            if line_number.last() == Some(&0) {
                line_number.pop();
            }

            let mut line = Vec::new();
            reader.read_until(b'\n', &mut line).await.map_err(|err| {
                WorktreeError::internal(format!("could not read git grep match: {err}"))
            })?;
            while matches!(line.last(), Some(b'\n' | b'\r')) {
                line.pop();
            }

            if matches.len() >= limit {
                truncated = true;
                let _ = child.kill().await;
                break;
            }

            let line_number = String::from_utf8_lossy(&line_number)
                .parse::<u32>()
                .unwrap_or(1);
            let path = String::from_utf8_lossy(&path)
                .trim_start_matches("./")
                .to_owned();
            let line = truncate_search_preview(&String::from_utf8_lossy(&line));
            matches.push(WorktreeSearchMatch {
                path,
                line_number,
                line,
            });
        }

        let status = child.wait().await.map_err(|err| {
            WorktreeError::internal(format!("could not wait for git grep: {err}"))
        })?;
        let mut stderr_bytes = Vec::new();
        stderr.read_to_end(&mut stderr_bytes).await.map_err(|err| {
            WorktreeError::internal(format!("could not read git grep stderr: {err}"))
        })?;
        // grep exits 1 when it found no matches. A deliberately killed process
        // is also expected once the response bound is known to be exceeded.
        if !status.success() && status.code() != Some(1) && !truncated {
            let message = String::from_utf8_lossy(&stderr_bytes).trim().to_owned();
            return Err(WorktreeError::user(if message.is_empty() {
                "workspace search failed".to_owned()
            } else {
                message
            }));
        }
        Ok((matches, truncated))
    })
    .await;

    match searched {
        Ok(result) => result,
        Err(_) => Err(WorktreeError::user("workspace search timed out")),
    }
}

fn search_limit(limit: u32) -> usize {
    let requested = if limit == 0 {
        DEFAULT_SEARCH_LIMIT
    } else {
        limit.min(MAX_SEARCH_LIMIT)
    };
    usize::try_from(requested).unwrap_or(DEFAULT_SEARCH_LIMIT as usize)
}

fn search_pathspecs(include: &str, exclude: &str) -> Vec<String> {
    let includes = search_globs(include)
        .into_iter()
        .map(|pattern| format!(":(glob){pattern}"))
        .collect::<Vec<_>>();
    let mut pathspecs = if includes.is_empty() {
        vec![".".to_owned()]
    } else {
        includes
    };
    pathspecs.extend(
        search_globs(exclude)
            .into_iter()
            .map(|pattern| format!(":(exclude,glob){pattern}")),
    );
    pathspecs
}

fn search_globs(spec: &str) -> Vec<String> {
    spec.split(',')
        .map(str::trim)
        .filter(|pattern| !pattern.is_empty())
        .map(|pattern| {
            pattern
                .replace('\\', "/")
                .trim_start_matches('/')
                .to_owned()
        })
        .filter(|pattern| !pattern.is_empty())
        .map(|pattern| {
            if pattern.contains('/') {
                pattern
            } else {
                format!("**/{pattern}")
            }
        })
        .collect()
}

fn truncate_search_preview(line: &str) -> String {
    let mut chars = line.chars();
    let preview = chars
        .by_ref()
        .take(MAX_SEARCH_PREVIEW_CHARS)
        .collect::<String>();
    if chars.next().is_some() {
        format!("{preview}…")
    } else {
        preview
    }
}

fn tree_limit(limit: u32) -> usize {
    let requested = if limit == 0 {
        DEFAULT_TREE_LIMIT
    } else {
        limit.min(MAX_TREE_LIMIT)
    };
    usize::try_from(requested).unwrap_or(DEFAULT_TREE_LIMIT as usize)
}

fn path_name_matches(path: &str, needle: &str) -> bool {
    let name = path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_ascii_lowercase();
    name.contains(needle) || path.to_ascii_lowercase().contains(needle)
}

/// Validate a path as a non-bare git repository and return its canonical toplevel.
pub(crate) async fn validate_repo_path(path: &Path) -> Result<ValidatedRepo, WorktreeError> {
    let requested = path.to_path_buf();
    if let Ok(bare) = git_stdout(
        Some(&requested),
        &["rev-parse", "--is-bare-repository"],
        GIT_TIMEOUT,
    )
    .await
    {
        if bare == "true" {
            return Err(WorktreeError::user(
                "bare repositories cannot be registered",
            ));
        }
    }
    let toplevel = git_stdout(
        Some(&requested),
        &["rev-parse", "--show-toplevel"],
        GIT_TIMEOUT,
    )
    .await
    .map_err(|err| WorktreeError::user(format!("not a git repository: {err}")))?;
    let toplevel = PathBuf::from(toplevel);
    let toplevel = toplevel.canonicalize().map_err(|err| {
        WorktreeError::user(format!(
            "could not canonicalize repository {}: {err}",
            toplevel.display()
        ))
    })?;
    Ok(ValidatedRepo { toplevel })
}

/// Create a worktree and branch under the Tidebreak data directory.
pub(crate) async fn create_worktree(
    repo_root: &Path,
    worktree_path: &Path,
    branch: &str,
    base_ref: &str,
) -> Result<WorktreeOperation, WorktreeError> {
    let expected_tip = resolve_ref(repo_root, base_ref).await?;
    let mut operation =
        reserve_worktree_target(repo_root, worktree_path, branch, &expected_tip).await?;
    if let Err(err) = create_branch_at(repo_root, branch, &expected_tip).await {
        operation.rollback().await;
        return Err(err);
    }
    operation.branch_created = true;
    if let Err(err) = operation.add_existing_branch().await {
        operation.rollback().await;
        return Err(err);
    }
    Ok(operation)
}

/// True when `refs/heads/<branch>` exists in the repository.
pub(crate) async fn branch_exists(repo_root: &Path, branch: &str) -> Result<bool, WorktreeError> {
    match git(
        Some(repo_root),
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
        GIT_TIMEOUT,
    )
    .await
    {
        Ok(_) => Ok(true),
        // --quiet: a missing ref exits non-zero with nothing on stderr; any
        // message is a real failure (not a repository, timeout, …).
        Err(err) if err.trim().is_empty() => Ok(false),
        Err(err) => Err(WorktreeError::internal(format!(
            "could not check branch {branch}: {err}"
        ))),
    }
}

/// Re-create a worktree checking out an existing branch.
///
/// The restore counterpart of [`create_worktree`], deliberately a sibling and
/// not a flag on it: create atomically mints a branch, while restore must never
/// mint one. The branch surviving archive is what makes the workspace worth
/// restoring.
pub(crate) async fn restore_worktree(
    repo_root: &Path,
    worktree_path: &Path,
    branch: &str,
) -> Result<WorktreeOperation, WorktreeError> {
    let expected_tip = branch_tip(repo_root, branch).await?;
    let mut operation =
        reserve_worktree_target(repo_root, worktree_path, branch, &expected_tip).await?;
    if let Err(err) = require_branch_tip(repo_root, branch, &expected_tip).await {
        operation.rollback().await;
        return Err(err);
    }
    if let Err(err) = operation.add_existing_branch().await {
        operation.rollback().await;
        return Err(err);
    }
    Ok(operation)
}

/// Re-create a released branch from its bundle and check out its exact tip.
pub(crate) async fn restore_released_worktree(
    repo_root: &Path,
    worktree_path: &Path,
    branch: &str,
    bundle: &Path,
    released_tip: &str,
) -> Result<WorktreeOperation, WorktreeError> {
    let mut operation =
        reserve_worktree_target(repo_root, worktree_path, branch, released_tip).await?;
    let restored = match restore_released_branch(&operation, bundle).await {
        Ok(restored) => restored,
        Err(err) => {
            operation.rollback().await;
            return Err(err);
        }
    };
    operation.branch_created = restored == RestoredBranch::Created;
    if let Err(err) = operation.add_existing_branch().await {
        operation.rollback().await;
        return Err(err);
    }
    Ok(operation)
}

impl WorktreeOperation {
    async fn add_existing_branch(&mut self) -> Result<(), WorktreeError> {
        let repo_root = &self.repo_root;
        let worktree_path = Path::new(&self.marker.worktree_path);
        let branch = self.marker.branch.as_str();
        self.require_repository_identity().await?;
        let add = git(
            Some(repo_root),
            &["worktree", "add", &worktree_path.to_string_lossy(), branch],
            GIT_WORKTREE_TIMEOUT,
        )
        .await;
        if let Err(err) = add {
            return Err(classify_worktree_add(err, branch));
        }
        self.write_registration_marker().await?;
        verify_worktree_identity(repo_root, worktree_path, branch, &self.marker.expected_tip).await
    }

    /// Release the reservation after the final workspace row is durable.
    pub(crate) async fn complete(self) {
        if let Err(error) = self.remove_registration_marker().await {
            tracing::warn!(
                path = %self.marker.worktree_path,
                operation = %self.marker.operation_id,
                "code-mode: could not release worktree registration marker: {error}"
            );
            return;
        }
        if let Err(error) = self.remove_owned_marker().await {
            tracing::warn!(
                path = %self.marker_path.display(),
                operation = %self.marker.operation_id,
                "code-mode: could not release worktree path marker: {error}"
            );
        }
    }

    /// Remove only the unchanged checkout and branch created by this attempt.
    pub(crate) async fn rollback(&self) {
        let owns_marker = match self.owns_marker().await {
            Ok(owns_marker) => owns_marker,
            Err(error) => {
                tracing::warn!(
                    path = %self.marker_path.display(),
                    operation = %self.marker.operation_id,
                    "code-mode: could not verify worktree operation ownership: {error}"
                );
                return;
            }
        };
        if !owns_marker {
            return;
        }

        if let Err(error) = self.require_repository_identity().await {
            tracing::warn!(
                repository = %self.repo_root.display(),
                operation = %self.marker.operation_id,
                "code-mode: refused worktree rollback after repository replacement: {error}"
            );
            let _ = self.remove_owned_marker().await;
            return;
        }

        let repo_root = &self.repo_root;
        let worktree_path = Path::new(&self.marker.worktree_path);
        let expected_branch = format!("refs/heads/{}", self.marker.branch);
        match registered_worktree(repo_root, worktree_path).await {
            Ok(Some(registered))
                if registered.head == self.marker.expected_tip
                    && registered.branch.as_deref() == Some(expected_branch.as_str()) =>
            {
                match self.owns_registration_marker().await {
                    Ok(true) => match git(
                        Some(repo_root),
                        &[
                            "worktree",
                            "remove",
                            "--force",
                            &registered.path.to_string_lossy(),
                        ],
                        GIT_WORKTREE_TIMEOUT,
                    )
                    .await
                    {
                        Ok(_) => {}
                        Err(error) => {
                            tracing::warn!(
                                path = %worktree_path.display(),
                                operation = %self.marker.operation_id,
                                "code-mode: could not roll back owned worktree: {error}"
                            );
                        }
                    },
                    Ok(false) => {}
                    Err(error) => {
                        tracing::warn!(
                            path = %worktree_path.display(),
                            operation = %self.marker.operation_id,
                            "code-mode: could not verify worktree registration ownership: {error}"
                        );
                    }
                }
            }
            Ok(None) => {}
            Ok(Some(_)) => {}
            Err(error) => {
                tracing::warn!(
                    path = %worktree_path.display(),
                    operation = %self.marker.operation_id,
                    "code-mode: could not inspect failed worktree registration: {error}"
                );
            }
        }

        let _ = prune_worktrees(repo_root).await;
        if self.branch_created
            && !branch_is_registered(repo_root, &self.marker.branch)
                .await
                .unwrap_or(true)
        {
            let branch_ref = format!("refs/heads/{}", self.marker.branch);
            let _ = git(
                Some(repo_root),
                &["update-ref", "-d", &branch_ref, &self.marker.expected_tip],
                GIT_TIMEOUT,
            )
            .await;
        }
        if let Err(error) = self.remove_owned_marker().await {
            tracing::warn!(
                path = %self.marker_path.display(),
                operation = %self.marker.operation_id,
                "code-mode: could not release failed worktree marker: {error}"
            );
        }
    }

    async fn require_repository_identity(&self) -> Result<(), WorktreeError> {
        let current = repository_identity(&self.repo_root).await?;
        if current == self.marker.repository {
            Ok(())
        } else {
            Err(WorktreeError::conflict(
                "worktree_repository_changed",
                format!(
                    "repository {} changed during worktree operation {}",
                    self.repo_root.display(),
                    self.marker.operation_id
                ),
            ))
        }
    }

    async fn write_registration_marker(&self) -> Result<(), WorktreeError> {
        let path = self.registration_marker_path().await?;
        let bytes = serde_json::to_vec(&self.marker).map_err(|error| {
            WorktreeError::internal(format!(
                "could not encode worktree registration marker: {error}"
            ))
        })?;
        let mut options = tokio::fs::OpenOptions::new();
        options.create_new(true).write(true);
        let mut file = options.open(&path).await.map_err(|error| {
            WorktreeError::internal(format!(
                "could not write worktree registration marker {}: {error}",
                path.display()
            ))
        })?;
        file.write_all(&bytes).await.map_err(|error| {
            WorktreeError::internal(format!(
                "could not write worktree registration marker {}: {error}",
                path.display()
            ))
        })?;
        file.sync_all().await.map_err(|error| {
            WorktreeError::internal(format!(
                "could not sync worktree registration marker {}: {error}",
                path.display()
            ))
        })
    }

    async fn registration_marker_path(&self) -> Result<PathBuf, WorktreeError> {
        let git_dir = git_stdout(
            Some(Path::new(&self.marker.worktree_path)),
            &["rev-parse", "--path-format=absolute", "--absolute-git-dir"],
            GIT_TIMEOUT,
        )
        .await
        .map_err(|error| {
            WorktreeError::internal(format!("could not resolve worktree git directory: {error}"))
        })?;
        Ok(PathBuf::from(git_dir.trim()).join(WORKTREE_REGISTRATION_MARKER))
    }

    async fn owns_registration_marker(&self) -> Result<bool, WorktreeError> {
        let path = self.registration_marker_path().await?;
        marker_matches(&path, &self.marker).await
    }

    async fn remove_registration_marker(&self) -> Result<(), WorktreeError> {
        let path = self.registration_marker_path().await?;
        if !marker_matches(&path, &self.marker).await? {
            return Err(WorktreeError::internal(format!(
                "worktree registration marker {} no longer belongs to operation {}",
                path.display(),
                self.marker.operation_id
            )));
        }
        tokio::fs::remove_file(&path).await.map_err(|error| {
            WorktreeError::internal(format!(
                "could not remove worktree registration marker {}: {error}",
                path.display()
            ))
        })
    }

    async fn owns_marker(&self) -> Result<bool, WorktreeError> {
        marker_matches(&self.marker_path, &self.marker).await
    }

    async fn remove_owned_marker(&self) -> Result<(), WorktreeError> {
        if !self.owns_marker().await? {
            return Ok(());
        }
        match tokio::fs::remove_file(&self.marker_path).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(WorktreeError::internal(format!(
                "could not remove operation marker {}: {error}",
                self.marker_path.display()
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RegisteredWorktree {
    path: PathBuf,
    head: String,
    branch: Option<String>,
}

async fn reserve_worktree_target(
    repo_root: &Path,
    worktree_path: &Path,
    branch: &str,
    expected_tip: &str,
) -> Result<WorktreeOperation, WorktreeError> {
    let repo_root = repo_root.canonicalize().map_err(|error| {
        WorktreeError::internal(format!(
            "could not canonicalize repository {}: {error}",
            repo_root.display()
        ))
    })?;
    let repository = repository_identity(&repo_root).await?;
    if let Some(parent) = worktree_path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|error| {
            WorktreeError::internal(format!(
                "could not create worktree parent {}: {error}",
                parent.display()
            ))
        })?;
    }
    let marker_path = worktree_operation_marker_path(worktree_path)?;
    let marker = WorktreeOperationMarker {
        operation_id: uuid::Uuid::new_v4(),
        repository,
        worktree_path: worktree_path.display().to_string(),
        branch: branch.to_owned(),
        expected_tip: expected_tip.to_owned(),
    };
    let bytes = serde_json::to_vec(&marker).map_err(|error| {
        WorktreeError::internal(format!(
            "could not encode worktree operation marker: {error}"
        ))
    })?;
    let mut options = tokio::fs::OpenOptions::new();
    options.create_new(true).write(true);
    let mut file = match options.open(&marker_path).await {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(WorktreeError::conflict(
                "worktree_path_busy",
                format!(
                    "another worktree operation owns {}",
                    worktree_path.display()
                ),
            ));
        }
        Err(error) => {
            return Err(WorktreeError::internal(format!(
                "could not reserve worktree path {}: {error}",
                worktree_path.display()
            )));
        }
    };
    if let Err(error) = file.write_all(&bytes).await {
        let _ = tokio::fs::remove_file(&marker_path).await;
        return Err(WorktreeError::internal(format!(
            "could not write worktree operation marker {}: {error}",
            marker_path.display()
        )));
    }
    if let Err(error) = file.sync_all().await {
        let _ = tokio::fs::remove_file(&marker_path).await;
        return Err(WorktreeError::internal(format!(
            "could not sync worktree operation marker {}: {error}",
            marker_path.display()
        )));
    }
    drop(file);
    let operation = WorktreeOperation {
        repo_root,
        marker_path,
        marker,
        branch_created: false,
    };
    match tokio::fs::symlink_metadata(worktree_path).await {
        Ok(_) => {
            operation.remove_owned_marker().await?;
            Err(WorktreeError::conflict(
                "worktree_path_occupied",
                format!("something already exists at {}", worktree_path.display()),
            ))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(operation),
        Err(error) => {
            operation.remove_owned_marker().await?;
            Err(WorktreeError::internal(format!(
                "could not inspect worktree path {}: {error}",
                worktree_path.display()
            )))
        }
    }
}

async fn repository_identity(repo_root: &Path) -> Result<String, WorktreeError> {
    let git_dir = git_stdout(
        Some(repo_root),
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        GIT_TIMEOUT,
    )
    .await
    .map_err(|error| {
        WorktreeError::internal(format!("could not resolve repository identity: {error}"))
    })?;
    Path::new(git_dir.trim())
        .canonicalize()
        .map(|path| path.display().to_string())
        .map_err(|error| {
            WorktreeError::internal(format!(
                "could not canonicalize repository identity {}: {error}",
                git_dir.trim()
            ))
        })
}

async fn marker_matches(
    path: &Path,
    expected: &WorktreeOperationMarker,
) -> Result<bool, WorktreeError> {
    match tokio::fs::read(path).await {
        Ok(bytes) => Ok(serde_json::from_slice::<WorktreeOperationMarker>(&bytes)
            .map(|marker| &marker == expected)
            .unwrap_or(false)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(WorktreeError::internal(format!(
            "could not read operation marker {}: {error}",
            path.display()
        ))),
    }
}

fn worktree_operation_marker_path(worktree_path: &Path) -> Result<PathBuf, WorktreeError> {
    let Some(file_name) = worktree_path.file_name() else {
        return Err(WorktreeError::internal(format!(
            "worktree path {} has no leaf",
            worktree_path.display()
        )));
    };
    let mut marker_name = OsString::from(".");
    marker_name.push(file_name);
    marker_name.push(WORKTREE_OPERATION_SUFFIX);
    Ok(worktree_path.with_file_name(marker_name))
}

async fn resolve_ref(repo_root: &Path, reference: &str) -> Result<String, WorktreeError> {
    git_stdout(
        Some(repo_root),
        &["rev-parse", &format!("{reference}^{{commit}}")],
        GIT_TIMEOUT,
    )
    .await
    .map(|tip| tip.trim().to_owned())
    .map_err(|error| WorktreeError::user(format!("could not resolve {reference}: {error}")))
}

async fn create_branch_at(
    repo_root: &Path,
    branch: &str,
    expected_tip: &str,
) -> Result<(), WorktreeError> {
    let branch_ref = format!("refs/heads/{branch}");
    git(
        Some(repo_root),
        &["update-ref", &branch_ref, expected_tip, ""],
        GIT_TIMEOUT,
    )
    .await
    .map(|_| ())
    .map_err(|error| {
        WorktreeError::conflict(
            "branch_collision",
            format!("branch {branch} already exists: {error}"),
        )
    })
}

async fn require_branch_tip(
    repo_root: &Path,
    branch: &str,
    expected_tip: &str,
) -> Result<(), WorktreeError> {
    let actual_tip = branch_tip(repo_root, branch).await?;
    if actual_tip == expected_tip {
        Ok(())
    } else {
        Err(WorktreeError::conflict(
            "branch_tip_changed",
            format!("branch {branch} moved from {expected_tip} to {actual_tip} during restore"),
        ))
    }
}

async fn verify_worktree_identity(
    repo_root: &Path,
    worktree_path: &Path,
    branch: &str,
    expected_tip: &str,
) -> Result<(), WorktreeError> {
    verify_inside_worktree(worktree_path).await?;
    let actual_repo = git_stdout(
        Some(worktree_path),
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        GIT_TIMEOUT,
    )
    .await
    .map_err(|error| {
        WorktreeError::internal(format!("worktree repository check failed: {error}"))
    })?;
    let expected_git_dir = git_stdout(
        Some(repo_root),
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        GIT_TIMEOUT,
    )
    .await
    .map_err(|error| {
        WorktreeError::internal(format!("repository identity check failed: {error}"))
    })?;
    if !repo_paths_equivalent(
        Path::new(actual_repo.trim()),
        Path::new(expected_git_dir.trim()),
    ) {
        return Err(WorktreeError::internal(format!(
            "worktree {} belongs to a different repository",
            worktree_path.display()
        )));
    }
    let head = resolve_ref(worktree_path, "HEAD").await?;
    if head != expected_tip {
        return Err(WorktreeError::conflict(
            "worktree_tip_changed",
            format!(
                "worktree {} checked out {head}, expected {expected_tip}",
                worktree_path.display()
            ),
        ));
    }
    let checked_out = git_stdout(
        Some(worktree_path),
        &["symbolic-ref", "--quiet", "HEAD"],
        GIT_TIMEOUT,
    )
    .await
    .map_err(|error| WorktreeError::internal(format!("worktree branch check failed: {error}")))?;
    if checked_out.trim() != format!("refs/heads/{branch}") {
        return Err(WorktreeError::internal(format!(
            "worktree {} checked out {}, expected {branch}",
            worktree_path.display(),
            checked_out.trim()
        )));
    }
    Ok(())
}

async fn registered_worktree(
    repo_root: &Path,
    worktree_path: &Path,
) -> Result<Option<RegisteredWorktree>, WorktreeError> {
    let fields = git_nul_stdout(
        Some(repo_root),
        &["worktree", "list", "--porcelain", "-z"],
        GIT_TIMEOUT,
    )
    .await
    .map_err(|error| WorktreeError::internal(format!("git worktree list failed: {error}")))?;
    let mut matches_path = false;
    let mut registered_path = None;
    let mut head = None;
    let mut branch = None;
    for field in fields {
        if let Some(path) = field.strip_prefix("worktree ") {
            if matches_path {
                return Ok(head.map(|head| RegisteredWorktree {
                    path: registered_path.expect("a matched worktree has a path"),
                    head,
                    branch,
                }));
            }
            let path = PathBuf::from(path);
            matches_path = existing_paths_equivalent(&path, worktree_path);
            registered_path = matches_path.then_some(path);
            head = None;
            branch = None;
        } else if matches_path {
            if let Some(value) = field.strip_prefix("HEAD ") {
                head = Some(value.to_owned());
            } else if let Some(value) = field.strip_prefix("branch ") {
                branch = Some(value.to_owned());
            }
        }
    }
    if matches_path {
        Ok(head.map(|head| RegisteredWorktree {
            path: registered_path.expect("a matched worktree has a path"),
            head,
            branch,
        }))
    } else {
        Ok(None)
    }
}

fn existing_paths_equivalent(left: &Path, right: &Path) -> bool {
    if repo_paths_equivalent(left, right) {
        return true;
    }
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => repo_paths_equivalent(&left, &right),
        _ => false,
    }
}

async fn branch_is_registered(repo_root: &Path, branch: &str) -> Result<bool, WorktreeError> {
    let expected = format!("branch refs/heads/{branch}");
    git_nul_stdout(
        Some(repo_root),
        &["worktree", "list", "--porcelain", "-z"],
        GIT_TIMEOUT,
    )
    .await
    .map(|fields| fields.iter().any(|field| field == &expected))
    .map_err(|error| WorktreeError::internal(format!("git worktree list failed: {error}")))
}

/// Run the setup script, if any. Failure preserves the checkout.
pub(crate) async fn run_setup_script(
    worktree_path: &Path,
    script: Option<&str>,
) -> Result<(), WorktreeError> {
    run_hook_script(worktree_path, script, "setup").await
}

/// Run the archive script, if any. Failure preserves the checkout, so the
/// caller must not remove the worktree when this returns an error.
pub(crate) async fn run_archive_script(
    worktree_path: &Path,
    script: Option<&str>,
) -> Result<(), WorktreeError> {
    run_hook_script(worktree_path, script, "archive").await
}

/// A non-zero exit is a failure, not a completed run: `run_workspace_script`
/// only reports `Err` when the script could not be spawned or timed out.
async fn run_hook_script(
    worktree_path: &Path,
    script: Option<&str>,
    label: &str,
) -> Result<(), WorktreeError> {
    let Some(script) = script.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(());
    };
    let run = run_workspace_script(worktree_path, script)
        .await
        .map_err(WorktreeError::user)?;
    if run.success {
        Ok(())
    } else {
        let truncation = if run.output_truncated {
            " [output truncated]"
        } else {
            ""
        };
        Err(WorktreeError::user(format!(
            "{label} script failed (exit {}): {}{truncation}",
            run.status
                .map(|code| code.to_string())
                .unwrap_or_else(|| "signal".into()),
            first_line(&run.stderr)
                .or_else(|| first_line(&run.stdout))
                .unwrap_or("no output")
        )))
    }
}

/// Whether the worktree has uncommitted or unpushed work that archive must not discard.
pub(crate) async fn archive_blockers(
    worktree_path: &Path,
    base_ref: &str,
) -> Result<Option<ArchiveBlock>, WorktreeError> {
    let uncommitted = has_uncommitted_work(worktree_path).await?;
    let unpushed = has_unpushed_work(worktree_path, base_ref).await?;
    let ignored = has_non_disposable_ignored_content(worktree_path).await?;
    Ok(if ignored {
        Some(ArchiveBlock::IgnoredContent)
    } else {
        match (uncommitted, unpushed) {
            (true, true) => Some(ArchiveBlock::UncommittedAndUnpushed),
            (true, false) => Some(ArchiveBlock::Uncommitted),
            (false, true) => Some(ArchiveBlock::Unpushed),
            (false, false) => None,
        }
    })
}

/// Remove a worktree, tolerating an already-gone checkout, then prune.
///
/// The branch is kept. `force` is required by the caller when the checkout
/// has uncommitted or unpushed work; this function uses `git worktree remove
/// --force` so a dirty tree does not block the removal itself.
pub(crate) async fn remove_worktree(
    repo_root: &Path,
    worktree_path: &Path,
) -> Result<(), WorktreeError> {
    let path = worktree_path.to_string_lossy();
    match git(
        Some(repo_root),
        &["worktree", "remove", "--force", path.as_ref()],
        GIT_WORKTREE_TIMEOUT,
    )
    .await
    {
        Ok(_) => {}
        Err(err) if already_gone(&err) => {}
        Err(err) => {
            // Directory may already be missing while git still lists it.
            if worktree_path.exists() {
                return Err(WorktreeError::internal(format!(
                    "git worktree remove failed: {err}"
                )));
            }
        }
    }
    prune_worktrees(repo_root).await
}

/// Where a released workspace's bundle lives.
///
/// Bundles are derived data, not user work: unlike a worktree (decision 53)
/// they belong in the disposable app-data directory, beside the database that
/// records them.
pub(crate) fn bundle_path(data_dir: &Path, workspace: &uuid::Uuid) -> PathBuf {
    data_dir
        .join("code")
        .join("bundles")
        .join(format!("{workspace}.bundle"))
}

/// The commit a branch points at.
pub(crate) async fn branch_tip(repo_root: &Path, branch: &str) -> Result<String, WorktreeError> {
    let sha = git_stdout(
        Some(repo_root),
        &["rev-parse", &format!("refs/heads/{branch}")],
        GIT_TIMEOUT,
    )
    .await
    .map_err(|err| WorktreeError::internal(format!("could not resolve {branch}: {err}")))?;
    Ok(sha.trim().to_owned())
}

/// Whether a released branch would strand commits nothing else can reach.
///
/// Release drops the branch, so the question archive asks about the checkout
/// is asked here about the history: are these commits merged into the base, or
/// would dropping the ref be the only copy going away? The bundle means the
/// answer is recoverable either way — this is what the confirmation is for,
/// not a correctness gate.
pub(crate) async fn release_is_unmerged(
    repo_root: &Path,
    base_ref: &str,
    branch: &str,
) -> Result<bool, WorktreeError> {
    let count = git_stdout(
        Some(repo_root),
        &[
            "rev-list",
            "--count",
            &format!("{base_ref}..refs/heads/{branch}"),
        ],
        GIT_TIMEOUT,
    )
    .await
    .map_err(|err| WorktreeError::internal(format!("git rev-list failed: {err}")))?;
    Ok(count.trim().parse::<u64>().unwrap_or(1) > 0)
}

/// Bundle a branch's own commits and return the file's size.
///
/// The range is `base..branch`, not the whole history: the base is still in
/// the repository, so carrying it would multiply the bundle by the size of the
/// project for nothing. What is left is the work this workspace did, which is
/// what a restore needs to put the branch back.
pub(crate) async fn create_bundle(
    repo_root: &Path,
    base_ref: &str,
    branch: &str,
    out: &Path,
) -> Result<u64, WorktreeError> {
    if let Some(parent) = out.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|err| {
            WorktreeError::internal(format!(
                "could not create bundle directory {}: {err}",
                parent.display()
            ))
        })?;
    }
    // `--` separates the revision range from the ref that names it, so the
    // bundle carries `refs/heads/<branch>` and a restore can fetch it by name.
    git(
        Some(repo_root),
        &[
            "bundle",
            "create",
            &out.to_string_lossy(),
            &format!("{base_ref}..refs/heads/{branch}"),
            &format!("refs/heads/{branch}"),
        ],
        GIT_WORKTREE_TIMEOUT,
    )
    .await
    .map_err(|err| WorktreeError::internal(format!("git bundle create failed: {err}")))?;
    let size = tokio::fs::metadata(out)
        .await
        .map_err(|err| {
            WorktreeError::internal(format!("could not stat bundle {}: {err}", out.display()))
        })?
        .len();
    Ok(size)
}

/// Restore the exact released branch tip through a temporary Tidebreak ref.
async fn restore_released_branch(
    operation: &WorktreeOperation,
    bundle: &Path,
) -> Result<RestoredBranch, WorktreeError> {
    let repo_root = &operation.repo_root;
    let branch = operation.marker.branch.as_str();
    let expected_tip = operation.marker.expected_tip.as_str();
    if !bundle.exists() {
        return Err(WorktreeError::user(format!(
            "bundle {} is missing; this workspace cannot be restored",
            bundle.display()
        )));
    }
    let path = bundle.to_string_lossy();
    git(
        Some(repo_root),
        &["bundle", "verify", path.as_ref()],
        GIT_WORKTREE_TIMEOUT,
    )
    .await
    .map_err(|err| WorktreeError::user(format!("bundle {path} is not usable: {err}")))?;

    let temporary_ref = format!(
        "refs/tidebreak/restores/{}",
        operation.marker.operation_id.simple()
    );
    let result = async {
        git(
            Some(repo_root),
            &[
                "fetch",
                path.as_ref(),
                &format!("refs/heads/{branch}:{temporary_ref}"),
            ],
            GIT_WORKTREE_TIMEOUT,
        )
        .await
        .map_err(|err| WorktreeError::user(format!("bundle does not contain {branch}: {err}")))?;

        let bundled_tip = resolve_ref(repo_root, &temporary_ref).await?;
        if bundled_tip != expected_tip {
            Err(WorktreeError::conflict(
                "released_tip_mismatch",
                format!(
                    "bundle for {branch} contains {bundled_tip}, expected released tip \
                         {expected_tip}"
                ),
            ))
        } else if branch_exists(repo_root, branch).await? {
            require_branch_tip(repo_root, branch, expected_tip)
                .await
                .map(|()| RestoredBranch::Existing)
                .map_err(|_| {
                    WorktreeError::conflict(
                        "released_branch_mismatch",
                        format!(
                            "branch {branch} exists at a different commit; the released \
                                 bundle was preserved"
                        ),
                    )
                })
        } else {
            match create_branch_at(repo_root, branch, expected_tip).await {
                Ok(()) => Ok(RestoredBranch::Created),
                Err(_) => require_branch_tip(repo_root, branch, expected_tip)
                    .await
                    .map(|()| RestoredBranch::Existing)
                    .map_err(|_| {
                        WorktreeError::conflict(
                            "released_branch_mismatch",
                            format!(
                                "branch {branch} was created at a different commit; the \
                                     released bundle was preserved"
                            ),
                        )
                    }),
            }
        }
    }
    .await;
    let _ = git(
        Some(repo_root),
        &["update-ref", "-d", &temporary_ref],
        GIT_TIMEOUT,
    )
    .await;
    result
}

/// Delete a branch, discarding the ref whether or not it merged.
///
/// Release has already bundled the commits, so `-D` is the honest flag: `-d`
/// would refuse exactly the unmerged branches release exists to reclaim.
pub(crate) async fn delete_branch(repo_root: &Path, branch: &str) -> Result<(), WorktreeError> {
    git(Some(repo_root), &["branch", "-D", branch], GIT_TIMEOUT)
        .await
        .map(|_| ())
        .map_err(|err| WorktreeError::internal(format!("git branch -D {branch} failed: {err}")))
}

/// Drop stale worktree registrations from the repo.
pub(crate) async fn prune_worktrees(repo_root: &Path) -> Result<(), WorktreeError> {
    git(Some(repo_root), &["worktree", "prune"], GIT_TIMEOUT)
        .await
        .map(|_| ())
        .map_err(|err| WorktreeError::internal(format!("git worktree prune failed: {err}")))
}

/// Slug used in data-dir paths and, with the repo prefix, in branch names.
pub(crate) fn slugify(value: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !slug.is_empty() {
            slug.push('-');
            last_dash = true;
        }
        if slug.len() >= 40 {
            break;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    slug
}

/// Branch name: repo prefix plus a slug of the title, or a two-word fallback.
pub(crate) fn branch_name(prefix: &str, title: &str, seed: u128) -> String {
    let prefix = if prefix.is_empty() {
        "tidebreak/".to_owned()
    } else if prefix.ends_with('/') {
        prefix.to_owned()
    } else {
        format!("{prefix}/")
    };
    let slug = slugify(title);
    let slug = if slug.is_empty() {
        two_word_name(seed)
    } else {
        slug
    };
    format!("{prefix}{slug}")
}

/// Path `<root>/<repo-slug>/<workspace-slug>-<short-id>/`.
///
/// The readable name leads and the id trails, because these paths are read by
/// people: they appear in terminal prompts, editor titles, and every `cd` an
/// agent narrates. The workspace id still carries uniqueness — two workspaces
/// on one repo may share a title, and a title may be empty — so it stays as a
/// short suffix rather than the leading segment it used to be.
///
/// The repo segment is the slug alone. Workspace ids are unique across every
/// repo, so two repos that share a name share a folder without ever sharing a
/// worktree.
pub(crate) fn worktree_dir(
    root: &Path,
    workspace_id: tidebreak_core::WorkspaceId,
    repo_slug: &str,
    workspace_slug: &str,
) -> PathBuf {
    root.join(dir_segment(repo_slug, "repo")).join(format!(
        "{}-{}",
        dir_segment(workspace_slug, "workspace"),
        short_id(workspace_id.as_uuid())
    ))
}

/// The default worktree root for an embedding that names no visible one:
/// `<data_dir>/code/worktrees`, where every worktree lived before the root
/// became configurable.
pub(crate) fn data_dir_worktree_root(data_dir: &Path) -> PathBuf {
    data_dir.join("code").join("worktrees")
}

/// First eight hex digits of a UUID — enough to separate the workspaces one
/// person runs, short enough to keep the path readable.
pub(crate) fn short_id(id: &uuid::Uuid) -> String {
    id.simple().to_string()[..8].to_owned()
}

fn dir_segment<'a>(slug: &'a str, fallback: &'a str) -> &'a str {
    if slug.is_empty() {
        fallback
    } else {
        slug
    }
}

pub(crate) fn two_word_name(seed: u128) -> String {
    let adjective = ADJECTIVES[(seed % ADJECTIVES.len() as u128) as usize];
    let noun = NOUNS[((seed / ADJECTIVES.len() as u128) % NOUNS.len() as u128) as usize];
    format!("{adjective}-{noun}")
}

const MAX_BLOB_BYTES: usize = 512 * 1_024;

/// One worktree file's text, bounded so a huge blob cannot fill the viewer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorktreeBlob {
    pub path: String,
    pub content: String,
    pub truncated: bool,
    pub binary: bool,
}

/// Read one relative worktree file as UTF-8 text.
pub(crate) async fn read_worktree_file(
    worktree_path: &Path,
    relative: &str,
) -> Result<WorktreeBlob, WorktreeError> {
    let rel = validate_relative_file(relative)?;
    let abs = worktree_path.join(&rel);
    let canonical_root = tokio::fs::canonicalize(worktree_path)
        .await
        .map_err(|err| WorktreeError::internal(format!("could not resolve the worktree: {err}")))?;
    let canonical = tokio::fs::canonicalize(&abs)
        .await
        .map_err(|_| WorktreeError::user(format!("file not found: {}", rel.display())))?;
    if !canonical.starts_with(&canonical_root) {
        return Err(WorktreeError::user("path must stay inside the worktree"));
    }
    let metadata = tokio::fs::metadata(&canonical).await.map_err(|err| {
        WorktreeError::internal(format!("could not read {}: {err}", rel.display()))
    })?;
    if !metadata.is_file() {
        return Err(WorktreeError::user(format!(
            "{} is not a file",
            rel.display()
        )));
    }
    let file = tokio::fs::File::open(&canonical).await.map_err(|err| {
        WorktreeError::internal(format!("could not read {}: {err}", rel.display()))
    })?;
    let mut bytes = Vec::new();
    let read = file
        .take((MAX_BLOB_BYTES as u64) + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|err| {
            WorktreeError::internal(format!("could not read {}: {err}", rel.display()))
        })?;
    let truncated = read > MAX_BLOB_BYTES;
    if truncated {
        bytes.truncate(MAX_BLOB_BYTES);
    }
    let path = rel.to_string_lossy().replace('\\', "/");
    if bytes.contains(&0) {
        return Ok(WorktreeBlob {
            path,
            content: String::new(),
            truncated: false,
            binary: true,
        });
    }
    Ok(WorktreeBlob {
        path,
        content: String::from_utf8_lossy(&bytes).into_owned(),
        truncated,
        binary: false,
    })
}

fn validate_relative_file(value: &str) -> Result<PathBuf, WorktreeError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(WorktreeError::user("path is required"));
    }
    let path = Path::new(trimmed);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(WorktreeError::user("path must be a relative worktree file"));
    }
    Ok(path.to_path_buf())
}

async fn verify_inside_worktree(path: &Path) -> Result<(), WorktreeError> {
    let inside = git_stdout(
        Some(path),
        &["rev-parse", "--is-inside-work-tree"],
        GIT_TIMEOUT,
    )
    .await
    .map_err(|err| WorktreeError::internal(format!("worktree verification failed: {err}")))?;
    if inside == "true" {
        Ok(())
    } else {
        Err(WorktreeError::internal(
            "worktree verification failed: path is not inside a work tree".to_owned(),
        ))
    }
}

async fn has_uncommitted_work(worktree_path: &Path) -> Result<bool, WorktreeError> {
    let status = git_stdout(Some(worktree_path), &["status", "--porcelain"], GIT_TIMEOUT)
        .await
        .map_err(|err| WorktreeError::internal(format!("git status failed: {err}")))?;
    Ok(!status.is_empty())
}

async fn has_non_disposable_ignored_content(worktree_path: &Path) -> Result<bool, WorktreeError> {
    let disposable = archive_disposable_paths(worktree_path).await?;
    let ignored = list_ignored_paths_bounded(worktree_path).await?;
    Ok(ignored
        .iter()
        .any(|path| !path_is_disposable(Path::new(path), &disposable)))
}

async fn archive_disposable_paths(worktree_path: &Path) -> Result<Vec<PathBuf>, WorktreeError> {
    let mut command = Command::new("git");
    command
        .args(["config", "--null", "--get-all", ARCHIVE_DISPOSABLE_PATH_KEY])
        .current_dir(worktree_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env("GIT_TERMINAL_PROMPT", "0");
    let child = command
        .spawn()
        .map_err(|error| WorktreeError::archive_uncertain(format!("git config failed: {error}")))?;
    let output = timeout(GIT_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| WorktreeError::archive_uncertain("git config timed out during archive"))?
        .map_err(|error| WorktreeError::archive_uncertain(format!("git config failed: {error}")))?;
    if output.status.code() == Some(1) && output.stdout.is_empty() {
        return Ok(Vec::new());
    }
    if !output.status.success() {
        return Err(WorktreeError::archive_uncertain(format!(
            "git config failed during archive: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    if output.stdout.len() > ARCHIVE_SCAN_MAX_PATH_BYTES {
        return Err(WorktreeError::archive_uncertain(
            "archive disposable-path configuration exceeded its path budget",
        ));
    }
    output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|value| !value.is_empty())
        .map(|value| validate_disposable_path(&String::from_utf8_lossy(value)))
        .collect()
}

fn validate_disposable_path(value: &str) -> Result<PathBuf, WorktreeError> {
    let path = Path::new(value.trim());
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(WorktreeError::archive_uncertain(format!(
            "{ARCHIVE_DISPOSABLE_PATH_KEY} must name a relative directory without . or ..: {value}"
        )));
    }
    Ok(path.to_path_buf())
}

fn path_is_disposable(path: &Path, disposable: &[PathBuf]) -> bool {
    disposable
        .iter()
        .any(|configured| path.starts_with(configured))
}

async fn list_ignored_paths_bounded(worktree_path: &Path) -> Result<Vec<String>, WorktreeError> {
    let mut command = Command::new("git");
    command
        .args([
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "-z",
        ])
        .current_dir(worktree_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env("GIT_TERMINAL_PROMPT", "0");
    let mut child = command.spawn().map_err(|error| {
        WorktreeError::archive_uncertain(format!("git ls-files failed: {error}"))
    })?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| WorktreeError::archive_uncertain("git ls-files did not open stdout"))?;
    let mut output = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let read = timeout(GIT_TIMEOUT, stdout.read(&mut chunk))
            .await
            .map_err(|_| WorktreeError::archive_uncertain("git ls-files timed out"))?
            .map_err(|error| {
                WorktreeError::archive_uncertain(format!("git ls-files failed: {error}"))
            })?;
        if read == 0 {
            break;
        }
        output.extend_from_slice(&chunk[..read]);
        if output.len() > ARCHIVE_SCAN_MAX_PATH_BYTES {
            let _ = child.kill().await;
            return Err(WorktreeError::archive_uncertain(format!(
                "ignored-content inspection exceeded its {}-byte path budget; configure generated directories with git config --add {ARCHIVE_DISPOSABLE_PATH_KEY} <directory>",
                ARCHIVE_SCAN_MAX_PATH_BYTES
            )));
        }
    }
    let status = timeout(GIT_TIMEOUT, child.wait())
        .await
        .map_err(|_| WorktreeError::archive_uncertain("git ls-files timed out"))?
        .map_err(|error| {
            WorktreeError::archive_uncertain(format!("git ls-files failed: {error}"))
        })?;
    if !status.success() {
        return Err(WorktreeError::archive_uncertain(
            "git ls-files failed during ignored-content inspection",
        ));
    }
    let paths = output
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect::<Vec<_>>();
    if paths.len() > ARCHIVE_SCAN_MAX_ENTRIES {
        return Err(WorktreeError::archive_uncertain(format!(
            "ignored-content inspection exceeded its {}-entry budget; configure generated directories with git config --add {ARCHIVE_DISPOSABLE_PATH_KEY} <directory>",
            ARCHIVE_SCAN_MAX_ENTRIES
        )));
    }
    Ok(paths)
}

async fn has_unpushed_work(worktree_path: &Path, base_ref: &str) -> Result<bool, WorktreeError> {
    match git_stdout(
        Some(worktree_path),
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
        GIT_TIMEOUT,
    )
    .await
    {
        Ok(_) => {
            let count = git_stdout(
                Some(worktree_path),
                &["rev-list", "--count", "@{u}..HEAD"],
                GIT_TIMEOUT,
            )
            .await
            .map_err(|err| WorktreeError::internal(format!("git rev-list failed: {err}")))?;
            Ok(count.parse::<u64>().unwrap_or(1) > 0)
        }
        Err(_) => {
            // No upstream: unique commits versus the workspace base would be lost
            // only if the branch were deleted; still report them so archive is honest.
            let range = format!("{base_ref}..HEAD");
            let count = git_stdout(
                Some(worktree_path),
                &["rev-list", "--count", &range],
                GIT_TIMEOUT,
            )
            .await
            .map_err(|err| WorktreeError::internal(format!("git rev-list failed: {err}")))?;
            Ok(count.parse::<u64>().unwrap_or(1) > 0)
        }
    }
}

fn classify_worktree_add(err: String, branch: &str) -> WorktreeError {
    let lower = err.to_ascii_lowercase();
    if lower.contains("already exists") || lower.contains("already used") {
        WorktreeError::user(format!("branch {branch} already exists"))
    } else {
        WorktreeError::user(format!("could not create worktree: {err}"))
    }
}

fn already_gone(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    lower.contains("not a working tree")
        || lower.contains("is not a working tree")
        || lower.contains("no such file")
        || lower.contains("does not exist")
}

fn first_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).find(|line| !line.is_empty())
}

struct GitOutput {
    stdout: String,
}

async fn git(cwd: Option<&Path>, args: &[&str], limit: Duration) -> Result<GitOutput, String> {
    let mut command = Command::new("git");
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env("GIT_TERMINAL_PROMPT", "0");
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let child = command
        .spawn()
        .map_err(|err| format!("failed to spawn git: {err}"))?;
    let output = timeout(limit, child.wait_with_output())
        .await
        .map_err(|_| format!("git {} timed out", args.join(" ")))?
        .map_err(|err| format!("git {} failed: {err}", args.join(" ")))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if output.status.success() {
        Ok(GitOutput { stdout })
    } else {
        Err(if stderr.is_empty() { stdout } else { stderr })
    }
}

async fn git_stdout(cwd: Option<&Path>, args: &[&str], limit: Duration) -> Result<String, String> {
    Ok(git(cwd, args, limit).await?.stdout)
}

async fn git_nul_stdout(
    cwd: Option<&Path>,
    args: &[&str],
    limit: Duration,
) -> Result<Vec<String>, String> {
    let mut command = Command::new("git");
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env("GIT_TERMINAL_PROMPT", "0");
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let child = command
        .spawn()
        .map_err(|err| format!("failed to spawn git: {err}"))?;
    let output = timeout(limit, child.wait_with_output())
        .await
        .map_err(|_| format!("git {} timed out", args.join(" ")))?
        .map_err(|err| format!("git {} failed: {err}", args.join(" ")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        return Err(if stderr.is_empty() { stdout } else { stderr });
    }
    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|chunk| !chunk.is_empty())
        .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;
    use tempfile::TempDir;

    fn init_repo() -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let repo = dir.path().join("origin");
        std::fs::create_dir_all(&repo).unwrap();
        run(&repo, &["git", "init", "-b", "main"]);
        run(&repo, &["git", "config", "user.email", "dev@example.com"]);
        run(&repo, &["git", "config", "user.name", "Dev"]);
        std::fs::write(repo.join("README.md"), "hello\n").unwrap();
        run(&repo, &["git", "add", "README.md"]);
        run(&repo, &["git", "commit", "-m", "init"]);
        (dir, repo)
    }

    fn scratch_worktree(data: &Path, label: &str) -> PathBuf {
        worktree_dir(
            &data_dir_worktree_root(data),
            tidebreak_core::WorkspaceId::new(),
            "demo",
            label,
        )
    }

    fn run(cwd: &Path, args: &[&str]) {
        let status = StdCommand::new(args[0])
            .args(&args[1..])
            .current_dir(cwd)
            .env("GIT_TERMINAL_PROMPT", "0")
            .status()
            .unwrap();
        assert!(status.success(), "{args:?} failed in {}", cwd.display());
    }

    async fn create_ready(repo: &Path, path: &Path, branch: &str, base_ref: &str) {
        create_worktree(repo, path, branch, base_ref)
            .await
            .unwrap()
            .complete()
            .await;
    }

    #[tokio::test]
    async fn validate_repo_refuses_bare_and_nested_non_repos() {
        let (dir, repo) = init_repo();
        let validated = validate_repo_path(&repo).await.unwrap();
        assert_eq!(validated.toplevel, repo.canonicalize().unwrap());

        let nested = repo.join("src");
        std::fs::create_dir_all(&nested).unwrap();
        let from_nested = validate_repo_path(&nested).await.unwrap();
        assert_eq!(from_nested.toplevel, validated.toplevel);

        let bare = dir.path().join("bare.git");
        run(
            dir.path(),
            &["git", "init", "--bare", bare.to_str().unwrap()],
        );
        let err = validate_repo_path(&bare).await.unwrap_err();
        assert!(err.to_string().contains("bare"), "{err}");
    }

    #[tokio::test]
    async fn create_verifies_and_setup_failure_preserves_checkout() {
        let (_dir, repo) = init_repo();
        let data = TempDir::new().unwrap();
        let path = scratch_worktree(data.path(), "first");
        create_ready(&repo, &path, "tidebreak/first", "main").await;
        verify_inside_worktree(&path).await.unwrap();

        let err = run_setup_script(&path, Some("exit 7")).await.unwrap_err();
        assert!(err.to_string().contains("setup script failed"), "{err}");
        assert!(path.join("README.md").is_file());
        verify_inside_worktree(&path).await.unwrap();
    }

    #[tokio::test]
    async fn setup_failure_reports_when_its_output_was_truncated() {
        let (_dir, repo) = init_repo();
        let data = TempDir::new().unwrap();
        let path = scratch_worktree(data.path(), "noisy-setup");
        create_worktree(&repo, &path, "tidebreak/noisy-setup", "main")
            .await
            .unwrap();

        // Windows runs workspace scripts through PowerShell, which cannot
        // parse the POSIX loop, so each platform floods from its own syntax.
        let noisy_script = if cfg!(windows) {
            "while ($true) { Write-Output '0123456789abcdef' }"
        } else {
            "while :; do printf '0123456789abcdef'; done"
        };
        let error = run_setup_script(&path, Some(noisy_script))
            .await
            .unwrap_err();

        assert!(error.to_string().contains("[output truncated]"), "{error}");
        assert!(path.join("README.md").is_file());
    }

    #[tokio::test]
    async fn create_cleans_up_a_half_created_worktree() {
        let (_dir, repo) = init_repo();
        let data = TempDir::new().unwrap();
        let path = scratch_worktree(data.path(), "ghost");
        // A missing base fails before branch or checkout creation.
        let err = create_worktree(&repo, &path, "tidebreak/ghost", "no-such-ref")
            .await
            .unwrap_err();
        assert!(!err.to_string().is_empty());
        assert!(!path.exists());
        let listed = git_stdout(Some(&repo), &["worktree", "list"], GIT_TIMEOUT)
            .await
            .unwrap();
        assert!(
            !listed.contains("ghost"),
            "stale worktree left behind: {listed}"
        );
    }

    #[tokio::test]
    async fn archive_refuses_dirty_work_without_force_and_prunes_gone_trees() {
        let (_dir, repo) = init_repo();
        let data = TempDir::new().unwrap();
        let path = scratch_worktree(data.path(), "dirty");
        create_ready(&repo, &path, "tidebreak/dirty", "main").await;
        std::fs::write(path.join("extra.txt"), "uncommitted\n").unwrap();
        assert_eq!(
            archive_blockers(&path, "main").await.unwrap(),
            Some(ArchiveBlock::Uncommitted)
        );

        remove_worktree(&repo, &path).await.unwrap();
        assert!(!path.exists());

        // Already-removed: deleting the directory out of band, then archive.
        let path2 = scratch_worktree(data.path(), "gone");
        create_ready(&repo, &path2, "tidebreak/gone", "main").await;
        std::fs::remove_dir_all(&path2).unwrap();
        remove_worktree(&repo, &path2).await.unwrap();
        let listed = git_stdout(Some(&repo), &["worktree", "list"], GIT_TIMEOUT)
            .await
            .unwrap();
        assert!(
            !listed.contains("gone"),
            "prune should drop the stale registration: {listed}"
        );
    }

    #[tokio::test]
    async fn branch_collision_is_a_user_visible_error() {
        let (_dir, repo) = init_repo();
        let data = TempDir::new().unwrap();
        let first = scratch_worktree(data.path(), "one");
        create_ready(&repo, &first, "tidebreak/same", "main").await;
        let second = scratch_worktree(data.path(), "two");
        let err = create_worktree(&repo, &second, "tidebreak/same", "main")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("already exists"),
            "collision must not auto-suffix: {err}"
        );
        assert!(!second.exists());
    }

    #[tokio::test]
    async fn occupied_foreign_directory_is_never_removed() {
        let (_dir, repo) = init_repo();
        let data = TempDir::new().unwrap();
        let path = scratch_worktree(data.path(), "foreign");
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("keep.txt"), "foreign\n").unwrap();

        let err = create_worktree(&repo, &path, "tidebreak/foreign", "main")
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            WorktreeError::Conflict {
                kind: "worktree_path_occupied",
                ..
            }
        ));
        assert_eq!(
            std::fs::read_to_string(path.join("keep.txt")).unwrap(),
            "foreign\n"
        );
        assert!(!branch_exists(&repo, "tidebreak/foreign").await.unwrap());
    }

    #[tokio::test]
    async fn concurrent_restores_leave_one_complete_checkout() {
        let (_dir, repo) = init_repo();
        run(&repo, &["git", "branch", "tidebreak/archive", "main"]);
        let data = TempDir::new().unwrap();
        let path = scratch_worktree(data.path(), "concurrent");

        let (left, right) = tokio::join!(
            restore_worktree(&repo, &path, "tidebreak/archive"),
            restore_worktree(&repo, &path, "tidebreak/archive")
        );
        let (winner, loser) = match (left, right) {
            (Ok(winner), Err(loser)) | (Err(loser), Ok(winner)) => (winner, loser),
            (left, right) => panic!("expected one restore winner, got {left:?} and {right:?}"),
        };
        assert!(matches!(
            loser,
            WorktreeError::Conflict {
                kind: "worktree_path_busy" | "worktree_path_occupied",
                ..
            }
        ));
        verify_inside_worktree(&path).await.unwrap();
        assert_eq!(
            branch_tip(&repo, "tidebreak/archive").await.unwrap(),
            resolve_ref(&path, "HEAD").await.unwrap()
        );
        winner.complete().await;
    }

    #[tokio::test]
    async fn losing_add_cleanup_preserves_the_concurrent_winner() {
        let (_dir, repo) = init_repo();
        let expected_tip = branch_tip(&repo, "main").await.unwrap();
        let data = TempDir::new().unwrap();
        let path = scratch_worktree(data.path(), "winner");
        let mut losing = reserve_worktree_target(&repo, &path, "tidebreak/loser", &expected_tip)
            .await
            .unwrap();
        create_branch_at(&repo, "tidebreak/loser", &expected_tip)
            .await
            .unwrap();
        losing.branch_created = true;

        run(&repo, &["git", "checkout", "-b", "tidebreak/winner"]);
        std::fs::write(repo.join("winner.txt"), "winner\n").unwrap();
        run(&repo, &["git", "add", "winner.txt"]);
        run(&repo, &["git", "commit", "-m", "winner"]);
        let winner_tip = branch_tip(&repo, "tidebreak/winner").await.unwrap();
        run(&repo, &["git", "checkout", "main"]);
        run(
            &repo,
            &[
                "git",
                "worktree",
                "add",
                path.to_str().unwrap(),
                "tidebreak/winner",
            ],
        );

        assert!(losing.add_existing_branch().await.is_err());
        losing.rollback().await;

        verify_inside_worktree(&path).await.unwrap();
        assert_eq!(resolve_ref(&path, "HEAD").await.unwrap(), winner_tip);
        assert!(path.join("winner.txt").is_file());
        assert!(branch_exists(&repo, "tidebreak/winner").await.unwrap());
        assert!(!branch_exists(&repo, "tidebreak/loser").await.unwrap());
    }

    #[tokio::test]
    async fn tree_listing_is_bounded_respects_ignore_and_never_returns_contents() {
        let (_dir, repo) = init_repo();
        std::fs::write(repo.join(".gitignore"), "secret.bin\nignored/\n").unwrap();
        std::fs::write(repo.join("src.rs"), "fn main() {}\n").unwrap();
        std::fs::write(repo.join("secret.bin"), "UNIQUE_PAYLOAD_xyz\n").unwrap();
        std::fs::create_dir_all(repo.join("ignored")).unwrap();
        std::fs::write(repo.join("ignored/hidden.rs"), "UNIQUE_PAYLOAD_xyz\n").unwrap();
        std::fs::write(repo.join("notes.md"), "UNIQUE_PAYLOAD_xyz\n").unwrap();
        run(&repo, &["git", "add", ".gitignore", "src.rs"]);
        run(&repo, &["git", "commit", "-m", "more files"]);

        let (paths, truncated) = list_tree_paths(&repo, "", 50).await.unwrap();
        assert!(!truncated);
        assert!(paths.iter().any(|path| path == "README.md"));
        assert!(paths.iter().any(|path| path == "src.rs"));
        assert!(paths.iter().any(|path| path == "notes.md"));
        assert!(paths.iter().any(|path| path == ".gitignore"));
        assert!(!paths.iter().any(|path| path.contains("secret")));
        assert!(!paths.iter().any(|path| path.contains("ignored")));
        let rendered = paths.join("\n");
        assert!(
            !rendered.contains("UNIQUE_PAYLOAD_xyz"),
            "tree listing leaked file contents: {rendered}"
        );

        let (matched, _) = list_tree_paths(&repo, "read", 50).await.unwrap();
        assert_eq!(matched, vec!["README.md".to_owned()]);

        for index in 0..250 {
            std::fs::write(repo.join(format!("bulk-{index:03}.txt")), "x\n").unwrap();
        }
        let (capped, truncated) = list_tree_paths(&repo, "bulk-", 50).await.unwrap();
        assert_eq!(capped.len(), 50);
        assert!(truncated);
        let (page, page_truncated) = list_tree_paths(&repo, "bulk-", 500).await.unwrap();
        assert_eq!(page.len(), 250);
        assert!(!page_truncated);
    }

    #[tokio::test]
    async fn content_search_matches_lines_and_honors_globs_and_bounds() {
        let (_dir, repo) = init_repo();
        std::fs::write(repo.join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(
            repo.join("src/lib.rs"),
            "fn crisp_search() {}\n// crisp second match\n",
        )
        .unwrap();
        std::fs::write(repo.join("notes.md"), "A crisp untracked note.\n").unwrap();
        std::fs::write(repo.join("ignored.txt"), "crisp secret\n").unwrap();
        run(&repo, &["git", "add", ".gitignore", "src/lib.rs"]);
        run(&repo, &["git", "commit", "-m", "search fixture"]);

        let (all, truncated) = search_worktree_contents(&repo, "CRISP", "", "", 50)
            .await
            .unwrap();
        assert!(!truncated);
        assert!(all.iter().any(|row| {
            row.path == "src/lib.rs" && row.line_number == 1 && row.line == "fn crisp_search() {}"
        }));
        assert!(all.iter().any(|row| row.path == "notes.md"));
        assert!(!all.iter().any(|row| row.path == "ignored.txt"));

        let (rust_only, _) = search_worktree_contents(&repo, "crisp", "*.rs", "", 50)
            .await
            .unwrap();
        assert!(rust_only.iter().all(|row| row.path.ends_with(".rs")));
        assert_eq!(rust_only.len(), 2);

        let (excluded, _) = search_worktree_contents(&repo, "crisp", "", "**/*.md", 50)
            .await
            .unwrap();
        assert!(!excluded.iter().any(|row| row.path.ends_with(".md")));

        let (bounded, bounded_truncated) = search_worktree_contents(&repo, "crisp", "", "", 1)
            .await
            .unwrap();
        assert_eq!(bounded.len(), 1);
        assert!(bounded_truncated);
    }

    /// The path a person reads: the repo folder and the workspace name lead,
    /// and the id is the short suffix that keeps two same-named workspaces
    /// apart. Two workspaces never share a directory even on one repo, and the
    /// root is whatever the deployment configured.
    #[test]
    fn worktree_paths_read_name_first_and_stay_unique() {
        let root = std::path::Path::new("/Users/sam/Tidebreak/workspaces");
        let first = tidebreak_core::WorkspaceId::new();
        let second = tidebreak_core::WorkspaceId::new();
        let left = worktree_dir(root, first, "tidebreak", "fix-login");
        let right = worktree_dir(root, second, "tidebreak", "fix-login");
        assert_ne!(left, right);
        assert_eq!(left.parent(), Some(root.join("tidebreak").as_path()));
        let leaf = left.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(leaf, format!("fix-login-{}", short_id(first.as_uuid())));
        // An untitled workspace on a nameless repo still resolves to two
        // segments rather than collapsing into the root.
        let bare = worktree_dir(root, first, "", "");
        assert_eq!(
            bare,
            root.join("repo")
                .join(format!("workspace-{}", short_id(first.as_uuid())))
        );
    }

    /// The headless default keeps every worktree where it has always lived.
    #[test]
    fn the_data_dir_root_is_unchanged() {
        assert_eq!(
            data_dir_worktree_root(std::path::Path::new("/srv/tidebreak")),
            std::path::Path::new("/srv/tidebreak/code/worktrees")
        );
    }

    #[test]
    fn untitled_workspaces_get_two_word_branch_names() {
        let name = branch_name("tidebreak/", "", 42);
        assert!(name.starts_with("tidebreak/"));
        let slug = name.strip_prefix("tidebreak/").unwrap();
        assert!(slug.contains('-'), "{slug}");
        assert_eq!(slugify("Hello, World!"), "hello-world");
    }

    #[tokio::test]
    async fn blob_reads_text_and_refuses_parent_paths() {
        let (_dir, repo) = init_repo();
        let data = TempDir::new().unwrap();
        let path = scratch_worktree(data.path(), "blob");
        create_ready(&repo, &path, "tidebreak/blob", "main").await;
        std::fs::write(path.join("notes.md"), "hello from blob\n").unwrap();

        let blob = read_worktree_file(&path, "notes.md").await.unwrap();
        assert_eq!(blob.path, "notes.md");
        assert_eq!(blob.content, "hello from blob\n");
        assert!(!blob.binary);
        assert!(!blob.truncated);

        let err = read_worktree_file(&path, "../README.md").await.unwrap_err();
        assert!(err.to_string().contains("relative"), "{err}");
    }

    #[tokio::test]
    async fn blob_read_is_capped_and_does_not_load_the_whole_file() {
        let (_dir, repo) = init_repo();
        let data = TempDir::new().unwrap();
        let path = scratch_worktree(data.path(), "blob-cap");
        create_ready(&repo, &path, "tidebreak/blob-cap", "main").await;
        let huge = format!("hello {}\n", "x".repeat(MAX_BLOB_BYTES + 64));
        std::fs::write(path.join("huge.txt"), &huge).unwrap();

        let blob = read_worktree_file(&path, "huge.txt").await.unwrap();
        assert_eq!(blob.path, "huge.txt");
        assert!(blob.truncated);
        assert!(!blob.binary);
        assert_eq!(blob.content.len(), MAX_BLOB_BYTES);
        assert!(blob.content.starts_with("hello "));
        assert_ne!(blob.content, huge);
    }

    #[cfg(windows)]
    #[test]
    fn windows_repo_identity_ignores_case_and_verbatim_presentation() {
        assert!(repo_paths_equivalent(
            Path::new(r"C:\Users\Dev\Repo"),
            Path::new(r"\\?\c:\users\dev\repo\")
        ));
        assert!(repo_paths_equivalent(
            Path::new(r"\\Server\Share\Repo"),
            Path::new(r"\\?\UNC\server\share\repo\")
        ));
        assert!(repo_paths_equivalent(
            Path::new(r"\\Server\Share"),
            Path::new(r"\\?\UNC\server\share\")
        ));
        assert!(!repo_paths_equivalent(
            Path::new(r"\\Server\Share\Repo"),
            Path::new(r"\\Server\Other\Repo")
        ));
    }

    /// The whole premise of the release tier: a bundle of `base..branch`
    /// carries the work, so dropping the branch is recoverable.
    #[tokio::test]
    async fn a_released_branch_round_trips_through_its_bundle() {
        let (dir, repo) = init_repo();
        run(&repo, &["git", "checkout", "-b", "tidebreak/work"]);
        std::fs::write(repo.join("feature.txt"), "work\n").unwrap();
        run(&repo, &["git", "add", "feature.txt"]);
        run(&repo, &["git", "commit", "-m", "add the feature"]);
        let tip = branch_tip(&repo, "tidebreak/work").await.unwrap();
        run(&repo, &["git", "checkout", "main"]);

        assert!(release_is_unmerged(&repo, "main", "tidebreak/work")
            .await
            .unwrap());

        let bundle = dir.path().join("work.bundle");
        let bytes = create_bundle(&repo, "main", "tidebreak/work", &bundle)
            .await
            .unwrap();
        assert!(bytes > 0);

        delete_branch(&repo, "tidebreak/work").await.unwrap();
        assert!(!branch_exists(&repo, "tidebreak/work").await.unwrap());

        let path = scratch_worktree(dir.path(), "released");
        restore_released_worktree(&repo, &path, "tidebreak/work", &bundle, &tip)
            .await
            .unwrap()
            .complete()
            .await;
        assert!(branch_exists(&repo, "tidebreak/work").await.unwrap());
        // Same commit, not merely a branch of the same name.
        assert_eq!(branch_tip(&repo, "tidebreak/work").await.unwrap(), tip);
        // Normalize: git checks out with CRLF under Windows' default
        // `core.autocrlf`, and the round trip is about the content, not the
        // platform's line endings.
        assert_eq!(
            std::fs::read_to_string(path.join("feature.txt"))
                .unwrap()
                .replace("\r\n", "\n"),
            "work\n"
        );
    }

    /// The bundle carries the branch's own commits, not the history behind
    /// them. This is what makes the tier worth having: the base is still in
    /// the repository, so shipping it again would scale the bundle with the
    /// project instead of with the work.
    #[tokio::test]
    async fn a_bundle_carries_only_the_branch_commits() {
        let (dir, repo) = init_repo();
        // Bulk on the base, which the bundle must not copy.
        std::fs::write(repo.join("big.bin"), "x".repeat(400_000)).unwrap();
        run(&repo, &["git", "add", "big.bin"]);
        run(&repo, &["git", "commit", "-m", "add bulk to the base"]);

        run(&repo, &["git", "checkout", "-b", "tidebreak/small"]);
        std::fs::write(repo.join("note.txt"), "one line\n").unwrap();
        run(&repo, &["git", "add", "note.txt"]);
        run(&repo, &["git", "commit", "-m", "add a note"]);
        run(&repo, &["git", "checkout", "main"]);

        let bundle = dir.path().join("small.bundle");
        let bytes = create_bundle(&repo, "main", "tidebreak/small", &bundle)
            .await
            .unwrap();
        assert!(
            bytes < 100_000,
            "bundle carried the base: {bytes} bytes for a one-line commit"
        );
    }

    /// A merged branch is the case release does not have to warn about.
    #[tokio::test]
    async fn a_merged_branch_is_not_reported_unmerged() {
        let (_dir, repo) = init_repo();
        run(&repo, &["git", "checkout", "-b", "tidebreak/merged"]);
        std::fs::write(repo.join("merged.txt"), "done\n").unwrap();
        run(&repo, &["git", "add", "merged.txt"]);
        run(&repo, &["git", "commit", "-m", "merged work"]);
        run(&repo, &["git", "checkout", "main"]);
        run(
            &repo,
            &["git", "merge", "--no-ff", "-m", "merge", "tidebreak/merged"],
        );

        assert!(!release_is_unmerged(&repo, "main", "tidebreak/merged")
            .await
            .unwrap());
    }

    /// A corrupt bundle must fail before it half-populates the object store.
    #[tokio::test]
    async fn a_corrupt_bundle_is_refused_and_leaves_no_branch() {
        let (dir, repo) = init_repo();
        let bundle = dir.path().join("broken.bundle");
        let path = scratch_worktree(dir.path(), "broken");
        std::fs::write(&bundle, b"not a bundle").unwrap();

        let err = restore_released_worktree(
            &repo,
            &path,
            "tidebreak/nope",
            &bundle,
            &branch_tip(&repo, "main").await.unwrap(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, WorktreeError::User(_)), "{err:?}");
        assert!(!branch_exists(&repo, "tidebreak/nope").await.unwrap());
        assert!(!path.exists());
        assert!(bundle.exists());
    }

    #[tokio::test]
    async fn released_restore_refuses_a_foreign_same_name_branch() {
        let (dir, repo) = init_repo();
        run(&repo, &["git", "checkout", "-b", "tidebreak/released"]);
        std::fs::write(repo.join("released.txt"), "released\n").unwrap();
        run(&repo, &["git", "add", "released.txt"]);
        run(&repo, &["git", "commit", "-m", "released"]);
        let released_tip = branch_tip(&repo, "tidebreak/released").await.unwrap();
        run(&repo, &["git", "checkout", "main"]);
        let bundle = dir.path().join("released.bundle");
        create_bundle(&repo, "main", "tidebreak/released", &bundle)
            .await
            .unwrap();
        delete_branch(&repo, "tidebreak/released").await.unwrap();

        run(&repo, &["git", "checkout", "-b", "tidebreak/released"]);
        std::fs::write(repo.join("foreign.txt"), "foreign\n").unwrap();
        run(&repo, &["git", "add", "foreign.txt"]);
        run(&repo, &["git", "commit", "-m", "foreign"]);
        let foreign_tip = branch_tip(&repo, "tidebreak/released").await.unwrap();
        run(&repo, &["git", "checkout", "main"]);

        let path = scratch_worktree(dir.path(), "foreign-branch");
        let err =
            restore_released_worktree(&repo, &path, "tidebreak/released", &bundle, &released_tip)
                .await
                .unwrap_err();
        assert!(matches!(
            err,
            WorktreeError::Conflict {
                kind: "released_branch_mismatch",
                ..
            }
        ));
        assert_eq!(
            branch_tip(&repo, "tidebreak/released").await.unwrap(),
            foreign_tip
        );
        assert!(bundle.exists());
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn released_restore_refuses_a_bundle_with_the_wrong_tip() {
        let (dir, repo) = init_repo();
        let expected_tip = branch_tip(&repo, "main").await.unwrap();
        run(&repo, &["git", "checkout", "-b", "tidebreak/released"]);
        std::fs::write(repo.join("wrong.txt"), "wrong\n").unwrap();
        run(&repo, &["git", "add", "wrong.txt"]);
        run(&repo, &["git", "commit", "-m", "wrong tip"]);
        run(&repo, &["git", "checkout", "main"]);
        let bundle = dir.path().join("wrong.bundle");
        create_bundle(&repo, "main", "tidebreak/released", &bundle)
            .await
            .unwrap();
        delete_branch(&repo, "tidebreak/released").await.unwrap();

        let path = scratch_worktree(dir.path(), "wrong-tip");
        let err =
            restore_released_worktree(&repo, &path, "tidebreak/released", &bundle, &expected_tip)
                .await
                .unwrap_err();
        assert!(matches!(
            err,
            WorktreeError::Conflict {
                kind: "released_tip_mismatch",
                ..
            }
        ));
        assert!(!branch_exists(&repo, "tidebreak/released").await.unwrap());
        assert!(bundle.exists());
        assert!(!path.exists());
    }
}
