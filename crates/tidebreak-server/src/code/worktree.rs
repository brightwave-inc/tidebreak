//! Git worktree operations for code-mode workspaces.
//!
//! Every git call is a bounded, non-interactive subprocess of the user's own
//! `git` binary. Arguments are an argv array, never a shell string.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;
use tokio::time::timeout;

use super::setup_script::run_workspace_script;

const GIT_TIMEOUT: Duration = Duration::from_secs(30);
const GIT_WORKTREE_TIMEOUT: Duration = Duration::from_secs(120);
/// Default number of paths the tree route returns.
pub(crate) const DEFAULT_TREE_LIMIT: u32 = 50;
/// Hard cap on the tree route. The explorer may request this many paths.
pub(crate) const MAX_TREE_LIMIT: u32 = 5_000;

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
}

impl ArchiveBlock {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Uncommitted => "uncommitted",
            Self::Unpushed => "unpushed",
            Self::UncommittedAndUnpushed => "uncommitted_and_unpushed",
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
}

impl WorktreeError {
    pub(crate) fn user(message: impl Into<String>) -> Self {
        Self::User(message.into())
    }

    fn internal(message: impl Into<String>) -> Self {
        Self::Internal(message.into())
    }
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
) -> Result<(), WorktreeError> {
    if let Some(parent) = worktree_path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|err| {
            WorktreeError::internal(format!(
                "could not create worktree parent {}: {err}",
                parent.display()
            ))
        })?;
    }
    let add = git(
        Some(repo_root),
        &[
            "worktree",
            "add",
            "-b",
            branch,
            &worktree_path.to_string_lossy(),
            base_ref,
        ],
        GIT_WORKTREE_TIMEOUT,
    )
    .await;
    if let Err(err) = add {
        cleanup_half_created(repo_root, worktree_path).await;
        return Err(classify_worktree_add(err, branch));
    }
    match verify_inside_worktree(worktree_path).await {
        Ok(()) => Ok(()),
        Err(err) => {
            cleanup_half_created(repo_root, worktree_path).await;
            Err(err)
        }
    }
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
        Err(WorktreeError::user(format!(
            "{label} script failed (exit {}): {}",
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
    Ok(match (uncommitted, unpushed) {
        (true, true) => Some(ArchiveBlock::UncommittedAndUnpushed),
        (true, false) => Some(ArchiveBlock::Uncommitted),
        (false, true) => Some(ArchiveBlock::Unpushed),
        (false, false) => None,
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

/// Path `<data_dir>/code/worktrees/<repo-slug>/<workspace-slug>/`.
/// `<data_dir>/code/worktrees/<repo-id>-<slug>/<workspace-id>-<slug>/`.
///
/// Ids are the uniqueness key so two clones of the same project cannot share
/// a directory. Slugs stay only as a human-readable suffix.
pub(crate) fn worktree_dir(
    data_dir: &Path,
    repo_id: tidebreak_core::RepoId,
    workspace_id: tidebreak_core::WorkspaceId,
    repo_slug: &str,
    workspace_slug: &str,
) -> PathBuf {
    data_dir
        .join("code")
        .join("worktrees")
        .join(dir_name(repo_id, repo_slug))
        .join(dir_name(workspace_id, workspace_slug))
}

fn dir_name(id: impl std::fmt::Display, slug: &str) -> String {
    if slug.is_empty() {
        id.to_string()
    } else {
        format!("{id}-{slug}")
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
    let bytes = tokio::fs::read(&canonical).await.map_err(|err| {
        WorktreeError::internal(format!("could not read {}: {err}", rel.display()))
    })?;
    let path = rel.to_string_lossy().replace('\\', "/");
    if bytes.contains(&0) {
        return Ok(WorktreeBlob {
            path,
            content: String::new(),
            truncated: false,
            binary: true,
        });
    }
    let truncated = bytes.len() > MAX_BLOB_BYTES;
    let end = bytes.len().min(MAX_BLOB_BYTES);
    Ok(WorktreeBlob {
        path,
        content: String::from_utf8_lossy(&bytes[..end]).into_owned(),
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

async fn cleanup_half_created(repo_root: &Path, worktree_path: &Path) {
    let path = worktree_path.to_string_lossy();
    let _ = git(
        Some(repo_root),
        &["worktree", "remove", "--force", path.as_ref()],
        GIT_TIMEOUT,
    )
    .await;
    let _ = git(Some(repo_root), &["worktree", "prune"], GIT_TIMEOUT).await;
    if worktree_path.exists() {
        let _ = tokio::fs::remove_dir_all(worktree_path).await;
    }
}

async fn has_uncommitted_work(worktree_path: &Path) -> Result<bool, WorktreeError> {
    let status = git_stdout(Some(worktree_path), &["status", "--porcelain"], GIT_TIMEOUT)
        .await
        .map_err(|err| WorktreeError::internal(format!("git status failed: {err}")))?;
    Ok(!status.is_empty())
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
            data,
            tidebreak_core::RepoId::new(),
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
        create_worktree(&repo, &path, "tidebreak/first", "main")
            .await
            .unwrap();
        verify_inside_worktree(&path).await.unwrap();

        let err = run_setup_script(&path, Some("exit 7")).await.unwrap_err();
        assert!(err.to_string().contains("setup script failed"), "{err}");
        assert!(path.join("README.md").is_file());
        verify_inside_worktree(&path).await.unwrap();
    }

    #[tokio::test]
    async fn create_cleans_up_a_half_created_worktree() {
        let (_dir, repo) = init_repo();
        let data = TempDir::new().unwrap();
        let path = scratch_worktree(data.path(), "ghost");
        // Point at a missing base so `worktree add` fails after creating the branch
        // attempt; cleanup must not leave a registered worktree.
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
        create_worktree(&repo, &path, "tidebreak/dirty", "main")
            .await
            .unwrap();
        std::fs::write(path.join("extra.txt"), "uncommitted\n").unwrap();
        assert_eq!(
            archive_blockers(&path, "main").await.unwrap(),
            Some(ArchiveBlock::Uncommitted)
        );

        remove_worktree(&repo, &path).await.unwrap();
        assert!(!path.exists());

        // Already-removed: deleting the directory out of band, then archive.
        let path2 = scratch_worktree(data.path(), "gone");
        create_worktree(&repo, &path2, "tidebreak/gone", "main")
            .await
            .unwrap();
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
        create_worktree(&repo, &first, "tidebreak/same", "main")
            .await
            .unwrap();
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

    #[test]
    fn worktree_paths_are_keyed_on_ids() {
        let data = std::path::Path::new("/tmp/data");
        let repo_a = tidebreak_core::RepoId::new();
        let repo_b = tidebreak_core::RepoId::new();
        let ws = tidebreak_core::WorkspaceId::new();
        let left = worktree_dir(data, repo_a, ws, "origin", "first");
        let right = worktree_dir(data, repo_b, ws, "origin", "first");
        assert_ne!(left, right);
        assert!(left.to_string_lossy().contains(&repo_a.to_string()));
        assert!(right.to_string_lossy().contains(&repo_b.to_string()));
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
        create_worktree(&repo, &path, "tidebreak/blob", "main")
            .await
            .unwrap();
        std::fs::write(path.join("notes.md"), "hello from blob\n").unwrap();

        let blob = read_worktree_file(&path, "notes.md").await.unwrap();
        assert_eq!(blob.path, "notes.md");
        assert_eq!(blob.content, "hello from blob\n");
        assert!(!blob.binary);
        assert!(!blob.truncated);

        let err = read_worktree_file(&path, "../README.md").await.unwrap_err();
        assert!(err.to_string().contains("relative"), "{err}");
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
}
