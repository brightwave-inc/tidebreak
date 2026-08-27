//! Work-in-progress preservation for supervised runs.
//!
//! A sandbox can be stopped at any moment — a spend ceiling, a wall-clock
//! bound, a cancel — and whatever sits uncommitted in its clones would die
//! with the pod. So after every successful turn and once more at stop, the
//! agent snapshots each cloned tree to a per-sandbox ref on the origin
//! remote. Dirty state is snapshotted with plumbing (`write-tree` +
//! `commit-tree`) that never moves HEAD, so the engine cannot observe the
//! snapshot commit on its branch no matter where the checkpoint fails.
//!
//! The ref names follow the supervising environment's published convention —
//! `mg-wip/<sandbox-id>-i<incarnation>` for the first repository, with an
//! `-r<position>` suffix for later ones — because its recovery tooling and a
//! successor sandbox resume from those refs.
//!
//! Everything here is deadline-bounded. A checkpoint shares one budget
//! across all trees, each git command gets the lesser of its class bound and
//! the tree's share, and a command that cannot start before the deadline
//! fails loudly instead of hanging the stop path.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::bootstrap::{ClonedRepository, Event};
use crate::trust::Trust;

/// Whole-checkpoint budget across every tree.
pub const CHECKPOINT_BUDGET: Duration = Duration::from_secs(600);
/// Bound for commands that walk the working tree (status, add).
const TREE_WALK_BOUND: Duration = Duration::from_secs(120);
/// Bound for commands that mutate refs or the index (commit, reset).
const MUTATION_BOUND: Duration = Duration::from_secs(30);
/// Bound for plumbing reads (rev-parse, rev-list, write-tree).
const PLUMBING_BOUND: Duration = Duration::from_secs(10);
/// Bound for the push itself.
const PUSH_BOUND: Duration = Duration::from_secs(180);
/// Ceiling for a diagnostic carried inside an event.
const DIAGNOSTIC_MAX_CHARS: usize = 4096;

/// Why this checkpoint ran; the payload carries it.
#[derive(Clone, Debug)]
pub enum CheckpointPoint {
    /// A turn just completed successfully.
    SuccessfulTurn {
        /// The turn that completed.
        turn: u32,
    },
    /// The run is stopping.
    Terminal {
        /// The stop reason.
        reason: String,
    },
}

impl CheckpointPoint {
    fn fields(&self) -> serde_json::Value {
        match self {
            Self::SuccessfulTurn { turn } => {
                serde_json::json!({ "checkpoint": "successful_turn", "turn": turn })
            }
            Self::Terminal { reason } => {
                serde_json::json!({ "checkpoint": "terminal", "terminal_reason": reason })
            }
        }
    }
}

/// One tree's checkpoint state across the run.
#[derive(Debug)]
struct TreeState {
    directory: PathBuf,
    position: usize,
    /// HEAD at capture, so a later detached or rebased head still pushes.
    initial_head: Option<String>,
    /// Last commit that reached the checkpoint ref.
    last_pushed_head: Option<String>,
    /// Fingerprint of the last pushed dirty state: (tree id, base head).
    last_pushed_dirty: Option<(String, String)>,
}

/// The checkpoint state for one sandbox incarnation.
#[derive(Debug)]
pub struct WipContext {
    sandbox_id: String,
    incarnation: u32,
    trust: Trust,
    trees: Vec<TreeState>,
}

/// The payload for `wip_push_unavailable`, announced once when policy denies
/// pushes: the deliverable file is the compensating channel.
#[must_use]
pub fn unavailable_payload() -> serde_json::Value {
    serde_json::json!({
        "reason": "push_denied_by_policy",
        "deliverable": crate::completion::TASK_OUTPUT_NAME,
    })
}

impl WipContext {
    /// Captures each clone's starting head and builds the context.
    pub async fn capture(
        sandbox_id: String,
        incarnation: u32,
        clones: &[ClonedRepository],
        trust: Trust,
    ) -> Self {
        let mut trees = Vec::new();
        for clone in clones {
            // Best-effort: a tree whose head cannot be read still gets
            // checkpointed, it just loses the moved-head shortcut.
            let initial_head = git(
                &trust,
                &clone.directory,
                PLUMBING_BOUND,
                Instant::now() + PLUMBING_BOUND,
                &["rev-parse", "HEAD"],
            )
            .await
            .ok()
            .map(|head| head.trim().to_owned());
            trees.push(TreeState {
                directory: clone.directory.clone(),
                position: clone.position,
                initial_head,
                last_pushed_head: None,
                last_pushed_dirty: None,
            });
        }
        Self {
            sandbox_id,
            incarnation,
            trust,
            trees,
        }
    }

    /// Checkpoints every tree, returning the events to report.
    ///
    /// A tree with nothing new to preserve produces no event. A tree that
    /// fails produces a `wip_push_failed` naming what went wrong, and the
    /// remaining trees still run — one broken clone must not cost the
    /// others their snapshot.
    pub async fn checkpoint(&mut self, point: &CheckpointPoint) -> Vec<Event> {
        let deadline = Instant::now() + CHECKPOINT_BUDGET;
        let total = self.trees.len();
        let mut events = Vec::new();
        for index in 0..total {
            // Re-divide what is left before every tree, so one slow tree
            // cannot starve the rest of the whole budget.
            let remaining = deadline.saturating_duration_since(Instant::now());
            let share =
                Instant::now() + remaining / u32::try_from(total - index).unwrap_or(1).max(1);
            let job = TreeJob {
                trust: &self.trust,
                reference: checkpoint_ref(
                    &self.sandbox_id,
                    self.incarnation,
                    self.trees[index].position,
                ),
                message: format!(
                    "tidebreak: preserve sandbox WIP {} i{}",
                    self.sandbox_id, self.incarnation
                ),
                point: point.fields(),
                share,
            };
            if let Some(event) = checkpoint_tree(&mut self.trees[index], &job).await {
                events.push(event);
            }
        }
        events
    }
}

/// The checkpoint ref for one repository position.
#[must_use]
pub fn checkpoint_ref(sandbox_id: &str, incarnation: u32, position: usize) -> String {
    let base = format!("mg-wip/{sandbox_id}-i{incarnation}");
    if position == 0 {
        base
    } else {
        format!("{base}-r{position}")
    }
}

/// Everything one tree's checkpoint needs besides its own state.
struct TreeJob<'a> {
    trust: &'a Trust,
    reference: String,
    message: String,
    point: serde_json::Value,
    share: Instant,
}

async fn checkpoint_tree(tree: &mut TreeState, job: &TreeJob<'_>) -> Option<Event> {
    let directory = tree.directory.clone();
    let status = match git(
        job.trust,
        &directory,
        TREE_WALK_BOUND,
        job.share,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )
    .await
    {
        Ok(status) => status,
        Err(error) => {
            return Some(failure_after_head_push(tree, job, "status_failed", &error).await)
        }
    };
    if !status.trim().is_empty() {
        return checkpoint_dirty_tree(tree, job, &directory).await;
    }

    let head = match git(
        job.trust,
        &directory,
        PLUMBING_BOUND,
        job.share,
        &["rev-parse", "HEAD"],
    )
    .await
    {
        Ok(head) => head.trim().to_owned(),
        Err(error) => {
            return Some(failure_event(
                tree,
                job,
                "head_unavailable",
                &error,
                None,
                None,
                None,
            ))
        }
    };

    let mut inspection_warning = None;
    let should_push = if tree.last_pushed_dirty.is_some() {
        // The remote ref still names a synthetic dirty snapshot. A later
        // clean tree supersedes that work even when HEAD itself never moved.
        true
    } else if tree.last_pushed_head.as_deref() == Some(head.as_str()) {
        false
    } else if tree
        .initial_head
        .as_deref()
        .is_some_and(|initial| initial != head)
    {
        true
    } else {
        // A clean tree still at its starting head may hold commits the turn
        // authored and then reset onto; only unpushed local commits count.
        match git(
            job.trust,
            &directory,
            PLUMBING_BOUND,
            job.share,
            &["rev-list", "--count", "HEAD", "--not", "--remotes"],
        )
        .await
        {
            Ok(count) => count.trim().parse::<u64>().map_or(true, |count| count > 0),
            Err(error) => {
                // When the question cannot be answered, push and say why the
                // snapshot may be redundant.
                inspection_warning = Some(compact_diagnostic(&error));
                true
            }
        }
    };
    if !should_push {
        return None;
    }

    match git(
        job.trust,
        &directory,
        PUSH_BOUND,
        job.share,
        &[
            "push",
            "--force",
            "--no-verify",
            "origin",
            &format!("HEAD:refs/heads/{}", job.reference),
        ],
    )
    .await
    {
        Ok(_) => {
            let mut payload = base_payload(tree, job);
            payload["commit"] = serde_json::json!(head);
            payload["created_commit"] = serde_json::json!(false);
            if let Some(warning) = inspection_warning {
                payload["inspection_warning"] = serde_json::json!(warning);
            }
            tree.last_pushed_dirty = None;
            tree.last_pushed_head = Some(head);
            Some(("wip_pushed".to_owned(), payload))
        }
        Err(error) => Some(failure_event(
            tree,
            job,
            "push_failed",
            &error,
            Some(&head),
            Some(false),
            None,
        )),
    }
}

/// Snapshots a dirty tree without ever moving HEAD.
///
/// The staged state becomes a tree object, `commit-tree` wraps it in a
/// commit parented on the current head, and the commit's sha is pushed
/// directly to the checkpoint ref. The task branch is untouched on every
/// path — success and failure alike — and the index is restored before the
/// push, so no push outcome can leave the engine's next turn looking at
/// staged state it did not stage.
async fn checkpoint_dirty_tree(
    tree: &mut TreeState,
    job: &TreeJob<'_>,
    directory: &Path,
) -> Option<Event> {
    let base_head = match git(
        job.trust,
        directory,
        PLUMBING_BOUND,
        job.share,
        &["rev-parse", "HEAD"],
    )
    .await
    {
        Ok(head) => head.trim().to_owned(),
        Err(error) => {
            return Some(failure_event(
                tree,
                job,
                "head_unavailable",
                &error,
                None,
                None,
                None,
            ))
        }
    };
    let index = match IndexSnapshot::capture(job, directory).await {
        Ok(index) => index,
        Err(error) => {
            return Some(failure_after_head_push(tree, job, "index_unavailable", &error).await)
        }
    };
    if let Err(error) = git(
        job.trust,
        directory,
        TREE_WALK_BOUND,
        job.share,
        &["add", "-A", "--"],
    )
    .await
    {
        let _ = index.restore();
        return Some(failure_after_head_push(tree, job, "stage_failed", &error).await);
    }
    // The tree id is both the snapshot's content and the dedup fingerprint:
    // the same dirty state pushed once need not be pushed again every turn.
    let tree_id = match git(
        job.trust,
        directory,
        PLUMBING_BOUND,
        job.share,
        &["write-tree"],
    )
    .await
    {
        Ok(id) => id.trim().to_owned(),
        Err(error) => {
            let _ = index.restore();
            return Some(failure_after_head_push(tree, job, "snapshot_failed", &error).await);
        }
    };
    if tree.last_pushed_dirty.as_ref() == Some(&(tree_id.clone(), base_head.clone())) {
        let _ = index.restore();
        return None;
    }
    let snapshot = match git(
        job.trust,
        directory,
        MUTATION_BOUND,
        job.share,
        &[
            "-c",
            "user.name=Tidebreak WIP",
            "-c",
            "user.email=tidebreak-wip@invalid",
            "-c",
            "commit.gpgsign=false",
            "commit-tree",
            "-p",
            &base_head,
            "-m",
            &job.message,
            &tree_id,
        ],
    )
    .await
    {
        Ok(sha) => sha.trim().to_owned(),
        Err(error) => {
            let _ = index.restore();
            return Some(failure_after_head_push(tree, job, "commit_failed", &error).await);
        }
    };
    let restored = index.restore().is_ok();
    match git(
        job.trust,
        directory,
        PUSH_BOUND,
        job.share,
        &[
            "push",
            "--force",
            "--no-verify",
            "origin",
            &format!("{snapshot}:refs/heads/{}", job.reference),
        ],
    )
    .await
    {
        Ok(_) => {
            let mut payload = base_payload(tree, job);
            payload["commit"] = serde_json::json!(snapshot);
            payload["created_commit"] = serde_json::json!(true);
            payload["task_branch_restored"] = serde_json::json!(restored);
            tree.last_pushed_dirty = Some((tree_id, base_head));
            tree.last_pushed_head = Some(snapshot);
            Some(("wip_pushed".to_owned(), payload))
        }
        Err(error) => Some(failure_event(
            tree,
            job,
            "push_failed",
            &error,
            Some(&snapshot),
            Some(false),
            None,
        )),
    }
}

/// The engine's index exactly as it stood before the snapshot staged
/// anything, restored by writing the bytes back. A `git reset` is not an
/// index-only restore: it aborts an in-progress merge, rebase, or
/// cherry-pick and flattens unmerged entries, so the engine's next turn
/// would resume in a repository state it never created.
struct IndexSnapshot {
    path: PathBuf,
    contents: Option<Vec<u8>>,
}

impl IndexSnapshot {
    async fn capture(job: &TreeJob<'_>, directory: &Path) -> Result<Self, String> {
        let raw = git(
            job.trust,
            directory,
            PLUMBING_BOUND,
            job.share,
            &["rev-parse", "--git-path", "index"],
        )
        .await?;
        let mut path = PathBuf::from(raw.trim());
        if path.is_relative() {
            path = directory.join(path);
        }
        let contents = match std::fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(format!(
                    "the index at {} could not be read: {error}",
                    path.display()
                ))
            }
        };
        Ok(Self { path, contents })
    }

    fn restore(&self) -> Result<(), String> {
        match &self.contents {
            Some(bytes) => std::fs::write(&self.path, bytes).map_err(|error| {
                format!(
                    "the index at {} could not be restored: {error}",
                    self.path.display()
                )
            }),
            None => match std::fs::remove_file(&self.path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(format!(
                    "the staged index at {} could not be removed: {error}",
                    self.path.display()
                )),
            },
        }
    }
}

/// A step failed mid-checkpoint; the last committed head may still be worth
/// preserving, so try to push it before reporting — unless the checkpoint
/// ref holds a dirty snapshot, whose work HEAD does not contain.
async fn failure_after_head_push(
    tree: &TreeState,
    job: &TreeJob<'_>,
    reason: &str,
    error: &str,
) -> Event {
    if tree.last_pushed_dirty.is_some() {
        let mut event = failure_event(tree, job, reason, error, None, Some(false), None);
        event.1["head_push_skipped"] = serde_json::json!("would_replace_dirty_snapshot");
        return event;
    }
    let head = match git(
        job.trust,
        &tree.directory,
        PLUMBING_BOUND,
        job.share,
        &["rev-parse", "HEAD"],
    )
    .await
    {
        Ok(head) => head.trim().to_owned(),
        Err(_) => return failure_event(tree, job, reason, error, None, None, None),
    };
    match git(
        job.trust,
        &tree.directory,
        PUSH_BOUND,
        job.share,
        &[
            "push",
            "--force",
            "--no-verify",
            "origin",
            &format!("{head}:refs/heads/{}", job.reference),
        ],
    )
    .await
    {
        Ok(_) => failure_event(tree, job, reason, error, Some(&head), Some(true), None),
        Err(push_error) => failure_event(
            tree,
            job,
            "push_failed",
            &push_error,
            Some(&head),
            Some(false),
            Some((reason, error)),
        ),
    }
}

fn base_payload(tree: &TreeState, job: &TreeJob<'_>) -> serde_json::Value {
    let mut payload = serde_json::json!({
        "ref": job.reference,
        "directory": tree.directory.display().to_string(),
    });
    if let Some(object) = payload.as_object_mut() {
        if let Some(fields) = job.point.as_object() {
            for (key, value) in fields {
                object.insert(key.clone(), value.clone());
            }
        }
    }
    payload
}

fn failure_event(
    tree: &TreeState,
    job: &TreeJob<'_>,
    reason: &str,
    error: &str,
    commit: Option<&str>,
    head_pushed: Option<bool>,
    preceded_by: Option<(&str, &str)>,
) -> Event {
    let mut payload = base_payload(tree, job);
    payload["reason"] = serde_json::json!(reason);
    payload["error"] = serde_json::json!(compact_diagnostic(error));
    if let Some(commit) = commit {
        payload["commit"] = serde_json::json!(commit);
    }
    if let Some(head_pushed) = head_pushed {
        payload["head_pushed"] = serde_json::json!(head_pushed);
    }
    if let Some((preceding_reason, preceding_error)) = preceded_by {
        payload["preceded_by"] = serde_json::json!(preceding_reason);
        payload["preceding_error"] = serde_json::json!(compact_diagnostic(preceding_error));
    }
    ("wip_push_failed".to_owned(), payload)
}

/// Cuts a diagnostic to what an event may reasonably carry.
fn compact_diagnostic(diagnostic: &str) -> String {
    if diagnostic.chars().count() <= DIAGNOSTIC_MAX_CHARS {
        return diagnostic.to_owned();
    }
    let mut compacted: String = diagnostic.chars().take(DIAGNOSTIC_MAX_CHARS).collect();
    compacted.push('…');
    compacted
}

/// Runs one git command under the lesser of its class bound and the tree's
/// share of the checkpoint budget.
async fn git(
    trust: &Trust,
    directory: &Path,
    class_bound: Duration,
    share_deadline: Instant,
    arguments: &[&str],
) -> Result<String, String> {
    let remaining = share_deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(format!(
            "git {} was not started: the checkpoint deadline is exhausted",
            arguments.join(" ")
        ));
    }
    let bound = class_bound.min(remaining);
    let mut command = tokio::process::Command::new("git");
    command
        .args(arguments)
        .current_dir(directory)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    for (name, value) in trust.environment() {
        command.env(name, value);
    }
    let output = tokio::time::timeout(bound, command.output())
        .await
        .map_err(|_| {
            format!(
                "git {} timed out after {} seconds",
                arguments.join(" "),
                bound.as_secs()
            )
        })?
        .map_err(|error| format!("git could not be started: {error}"))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let diagnostic = if stderr.trim().is_empty() {
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    } else {
        stderr.trim().to_owned()
    };
    Err(format!("git exited with {}: {diagnostic}", output.status))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn refs_follow_the_published_convention() {
        assert_eq!(checkpoint_ref("sb-1", 1, 0), "mg-wip/sb-1-i1");
        assert_eq!(checkpoint_ref("sb-1", 3, 2), "mg-wip/sb-1-i3-r2");
    }

    fn run(directory: &Path, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .args(arguments)
            .current_dir(directory)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    /// A source repository with one commit, and a clone of it whose origin
    /// accepts pushes to checkpoint refs.
    fn fixture(root: &Path) -> PathBuf {
        let origin = root.join("origin");
        std::fs::create_dir_all(&origin).unwrap();
        run(&origin, &["init", "--initial-branch=main"]);
        std::fs::write(origin.join("README.md"), "hello\n").unwrap();
        run(&origin, &["add", "-A"]);
        run(
            &origin,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.invalid",
                "commit",
                "-m",
                "first",
            ],
        );
        let clone = root.join("clone");
        run(
            root,
            &[
                "clone",
                &origin.display().to_string(),
                &clone.display().to_string(),
            ],
        );
        clone
    }

    fn trust(root: &Path) -> Trust {
        Trust {
            bundle: root.join("bundle.pem"),
            certificate: root.join("ca.crt"),
            merged_system_roots: false,
        }
    }

    async fn context(root: &Path, clone: &Path) -> WipContext {
        WipContext::capture(
            "sb-1".to_owned(),
            1,
            &[ClonedRepository {
                directory: clone.to_path_buf(),
                position: 0,
            }],
            trust(root),
        )
        .await
    }

    #[tokio::test]
    async fn a_dirty_tree_is_committed_pushed_and_restored() {
        let root = tempfile::tempdir().unwrap();
        let clone = fixture(root.path());
        let mut context = context(root.path(), &clone).await;
        let base = run(&clone, &["rev-parse", "HEAD"]);
        std::fs::write(clone.join("draft.txt"), "unfinished\n").unwrap();

        let events = context
            .checkpoint(&CheckpointPoint::SuccessfulTurn { turn: 2 })
            .await;
        assert_eq!(events.len(), 1);
        let (kind, payload) = &events[0];
        assert_eq!(kind, "wip_pushed");
        assert_eq!(payload["ref"], "mg-wip/sb-1-i1");
        assert_eq!(payload["checkpoint"], "successful_turn");
        assert_eq!(payload["turn"], 2);
        assert_eq!(payload["created_commit"], true);
        assert_eq!(payload["task_branch_restored"], true);

        // The snapshot reached origin and carries the dirty file.
        let origin = root.path().join("origin");
        let pushed = run(&origin, &["rev-parse", "refs/heads/mg-wip/sb-1-i1"]);
        assert_eq!(payload["commit"], pushed);
        let listing = run(&origin, &["ls-tree", "--name-only", &pushed]);
        assert!(listing.contains("draft.txt"));

        // The task branch is back where the engine left it, file intact.
        assert_eq!(run(&clone, &["rev-parse", "HEAD"]), base);
        assert!(clone.join("draft.txt").is_file());
    }

    #[tokio::test]
    async fn an_unchanged_tree_pushes_nothing() {
        let root = tempfile::tempdir().unwrap();
        let clone = fixture(root.path());
        let mut context = context(root.path(), &clone).await;
        let events = context
            .checkpoint(&CheckpointPoint::SuccessfulTurn { turn: 1 })
            .await;
        assert!(events.is_empty());
    }

    /// The second checkpoint of the same dirty state must be silent: the
    /// snapshot already exists and re-pushing it every turn is churn.
    #[tokio::test]
    async fn the_same_dirty_state_is_not_pushed_twice() {
        let root = tempfile::tempdir().unwrap();
        let clone = fixture(root.path());
        let mut context = context(root.path(), &clone).await;
        std::fs::write(clone.join("draft.txt"), "unfinished\n").unwrap();
        let first = context
            .checkpoint(&CheckpointPoint::SuccessfulTurn { turn: 1 })
            .await;
        assert_eq!(first.len(), 1);
        let second = context
            .checkpoint(&CheckpointPoint::SuccessfulTurn { turn: 2 })
            .await;
        assert!(second.is_empty());

        // New work resumes pushing.
        std::fs::write(clone.join("draft.txt"), "more\n").unwrap();
        let third = context
            .checkpoint(&CheckpointPoint::Terminal {
                reason: "expired".to_owned(),
            })
            .await;
        assert_eq!(third.len(), 1);
        assert_eq!(third[0].1["terminal_reason"], "expired");
    }

    #[tokio::test]
    async fn a_clean_tree_supersedes_the_last_dirty_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let clone = fixture(root.path());
        let mut context = context(root.path(), &clone).await;
        let head = run(&clone, &["rev-parse", "HEAD"]);
        std::fs::write(clone.join("draft.txt"), "discard me\n").unwrap();

        let dirty = context
            .checkpoint(&CheckpointPoint::SuccessfulTurn { turn: 1 })
            .await;
        assert_eq!(dirty.len(), 1);
        assert_eq!(dirty[0].1["created_commit"], true);
        std::fs::remove_file(clone.join("draft.txt")).unwrap();

        let clean = context
            .checkpoint(&CheckpointPoint::SuccessfulTurn { turn: 2 })
            .await;
        assert_eq!(clean.len(), 1);
        assert_eq!(clean[0].1["created_commit"], false);
        assert_eq!(clean[0].1["commit"], head);

        let origin = root.path().join("origin");
        assert_eq!(
            run(&origin, &["rev-parse", "refs/heads/mg-wip/sb-1-i1"]),
            head
        );
        assert!(context
            .checkpoint(&CheckpointPoint::SuccessfulTurn { turn: 3 })
            .await
            .is_empty());
    }

    /// A clean tree whose head moved — the engine committed — still pushes,
    /// without creating a snapshot commit.
    #[tokio::test]
    async fn an_authored_commit_is_pushed_without_a_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let clone = fixture(root.path());
        let mut context = context(root.path(), &clone).await;
        std::fs::write(clone.join("work.txt"), "done\n").unwrap();
        run(&clone, &["add", "-A"]);
        run(
            &clone,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.invalid",
                "commit",
                "-m",
                "real work",
            ],
        );
        let head = run(&clone, &["rev-parse", "HEAD"]);
        let events = context
            .checkpoint(&CheckpointPoint::SuccessfulTurn { turn: 1 })
            .await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].1["created_commit"], false);
        assert_eq!(events[0].1["commit"], head);
        // The engine's own commit stays on the branch.
        assert_eq!(run(&clone, &["rev-parse", "HEAD"]), head);
    }

    /// A mid-checkpoint failure must not push HEAD over a dirty snapshot the
    /// ref already preserves: HEAD does not contain that work.
    #[tokio::test]
    async fn a_failure_never_pushes_head_over_a_dirty_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let clone = fixture(root.path());
        let mut context = context(root.path(), &clone).await;
        std::fs::write(clone.join("draft.txt"), "unfinished\n").unwrap();
        let events = context
            .checkpoint(&CheckpointPoint::SuccessfulTurn { turn: 1 })
            .await;
        let snapshot = events[0].1["commit"].as_str().unwrap().to_owned();
        let origin = root.path().join("origin");
        assert_eq!(
            run(&origin, &["rev-parse", "refs/heads/mg-wip/sb-1-i1"]),
            snapshot
        );

        let trust = trust(root.path());
        let job = TreeJob {
            trust: &trust,
            reference: checkpoint_ref("sb-1", 1, 0),
            message: "m".to_owned(),
            point: serde_json::json!({}),
            share: Instant::now() + CHECKPOINT_BUDGET,
        };
        let (kind, payload) =
            failure_after_head_push(&context.trees[0], &job, "status_failed", "boom").await;
        assert_eq!(kind, "wip_push_failed");
        assert_eq!(payload["reason"], "status_failed");
        assert_eq!(payload["head_pushed"], false);
        assert_eq!(payload["head_push_skipped"], "would_replace_dirty_snapshot");
        // The snapshot still owns the ref.
        assert_eq!(
            run(&origin, &["rev-parse", "refs/heads/mg-wip/sb-1-i1"]),
            snapshot
        );
    }

    /// A checkpoint during a conflicted merge must hand the merge back
    /// exactly as the engine left it: MERGE_HEAD intact, entries still
    /// unmerged. A reset-based restore aborts the merge instead.
    #[tokio::test]
    async fn a_conflicted_merge_survives_the_checkpoint() {
        let root = tempfile::tempdir().unwrap();
        let clone = fixture(root.path());
        run(&clone, &["checkout", "-b", "side"]);
        std::fs::write(clone.join("README.md"), "side\n").unwrap();
        run(&clone, &["add", "-A"]);
        run(
            &clone,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.invalid",
                "commit",
                "-m",
                "side",
            ],
        );
        run(&clone, &["checkout", "main"]);
        std::fs::write(clone.join("README.md"), "main\n").unwrap();
        run(&clone, &["add", "-A"]);
        run(
            &clone,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.invalid",
                "commit",
                "-m",
                "main",
            ],
        );
        // The merge needs an identity even to conflict: without one git
        // dies before touching the tree (CI runners auto-detect none).
        let merge = Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.invalid",
                "merge",
                "side",
            ])
            .current_dir(&clone)
            .output()
            .unwrap();
        assert!(!merge.status.success(), "the merge must conflict");
        assert!(
            clone.join(".git").join("MERGE_HEAD").is_file(),
            "the merge died without conflicting: {}",
            String::from_utf8_lossy(&merge.stderr)
        );
        let head = run(&clone, &["rev-parse", "HEAD"]);
        let mut context = context(root.path(), &clone).await;

        let events = context
            .checkpoint(&CheckpointPoint::SuccessfulTurn { turn: 1 })
            .await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, "wip_pushed");
        assert_eq!(events[0].1["created_commit"], true);
        assert_eq!(events[0].1["task_branch_restored"], true);

        // The merge is still in progress, entries still unmerged.
        assert_eq!(run(&clone, &["rev-parse", "HEAD"]), head);
        assert!(clone.join(".git").join("MERGE_HEAD").is_file());
        assert!(run(&clone, &["status", "--porcelain"]).contains("UU README.md"));
    }

    #[tokio::test]
    async fn a_failed_push_reports_instead_of_hiding() {
        let root = tempfile::tempdir().unwrap();
        let clone = fixture(root.path());
        // Break the remote so the push must fail.
        run(
            &clone,
            &["remote", "set-url", "origin", "/nonexistent/origin"],
        );
        let mut context = context(root.path(), &clone).await;
        let base = run(&clone, &["rev-parse", "HEAD"]);
        std::fs::write(clone.join("draft.txt"), "unfinished\n").unwrap();
        let events = context
            .checkpoint(&CheckpointPoint::Terminal {
                reason: "cancelled".to_owned(),
            })
            .await;
        assert_eq!(events.len(), 1);
        let (kind, payload) = &events[0];
        assert_eq!(kind, "wip_push_failed");
        assert_eq!(payload["reason"], "push_failed");
        assert_eq!(payload["head_pushed"], false);
        assert!(payload["error"].as_str().unwrap().contains("git exited"));

        // The failed push must not leave the engine on the snapshot commit,
        // and the staging done for the snapshot must be undone.
        assert_eq!(run(&clone, &["rev-parse", "HEAD"]), base);
        assert!(clone.join("draft.txt").is_file());
        assert!(run(&clone, &["status", "--porcelain"]).contains("?? draft.txt"));
    }
}
