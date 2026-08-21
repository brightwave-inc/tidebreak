//! Bounded `git clone` jobs for adding a remote repository.
//!
//! The user's own `git` binary does the work. Arguments are an argv array,
//! never a shell string. Credential helpers may authenticate; the process
//! never prompts and never stores secrets.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use tokio::io::{AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio::time::timeout;
use uuid::Uuid;

use tidebreak_core::{OwnerId, RepoId, Store};

use super::bus::{CloneProgress, CodeLiveUpdate};
use super::gh::{self, resolve_github_clone_url};
use super::runtime::CodeRuntime;
use crate::error::ServerError;
use crate::routes::code::{CodeCloneDefaults, CodeCloneJobSnapshot};

const CLONE_TIMEOUT: Duration = Duration::from_secs(900);
const MAX_STDERR_CHARS: usize = 4_096;
pub(crate) const CLONE_PARENT_DIR_SETTING: &str = "code_clone_parent_dir";

/// In-memory clone jobs for this process. Not journaled; a restart drops them.
#[derive(Debug, Default)]
pub(crate) struct CloneJobs {
    jobs: Mutex<std::collections::HashMap<Uuid, CloneJob>>,
}

#[derive(Debug, Clone)]
struct CloneJob {
    id: Uuid,
    phase: String,
    percent: Option<u8>,
    done: bool,
    error: Option<String>,
    repo_id: Option<RepoId>,
}

impl CloneJobs {
    fn snapshot(&self, id: Uuid) -> Option<CodeCloneJobSnapshot> {
        self.jobs
            .lock()
            .expect("clone jobs")
            .get(&id)
            .map(CloneJob::to_snapshot)
    }

    fn insert(&self, job: CloneJob) {
        self.jobs.lock().expect("clone jobs").insert(job.id, job);
    }

    fn apply(&self, id: Uuid, update: impl FnOnce(&mut CloneJob)) -> Option<CloneJob> {
        let mut guard = self.jobs.lock().expect("clone jobs");
        let job = guard.get_mut(&id)?;
        update(job);
        Some(job.clone())
    }
}

impl CloneJob {
    fn to_snapshot(&self) -> CodeCloneJobSnapshot {
        CodeCloneJobSnapshot {
            id: self.id.to_string(),
            phase: self.phase.clone(),
            percent: self.percent,
            done: self.done,
            error: self.error.clone(),
            repo_id: self.repo_id,
        }
    }
}

/// Directory a given owner's checkouts land in, under a deployment-wide
/// parent: the clone parent here, and the worktree root in
/// [`super::worktree_root`].
///
/// Each of those parents is one setting shared by every principal, so without
/// a per-owner segment two users cloning the same remote would both target
/// `<parent>/<name>` and the second would be refused — or worse, adopt the
/// first user's checkout. The local profile keeps writing straight into the
/// parent, because there is exactly one owner and its paths are the ones users
/// already have.
///
/// Owner keys are visible ASCII and may contain characters that are not safe
/// in a path segment, so everything outside a conservative set is replaced.
pub(crate) fn owner_dir(parent: &Path, owner: &OwnerId) -> PathBuf {
    if owner.is_local() {
        return parent.to_path_buf();
    }
    let segment: String = owner
        .as_str()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    // A key of only separators must not resolve to the parent itself, and
    // must never climb out of it.
    let segment = segment.trim_matches('.').to_owned();
    if segment.is_empty() {
        parent.join("owner")
    } else {
        parent.join(segment)
    }
}

/// Body fields after the route has rejected an empty or mixed source.
pub(crate) struct CloneRequest {
    pub url: Option<String>,
    pub github: Option<String>,
    pub parent_dir: String,
    pub name: Option<String>,
}

impl CodeRuntime {
    pub(crate) async fn clone_defaults(&self) -> Result<CodeCloneDefaults, ServerError> {
        let parent_dir = read_clone_parent_dir(&*self.db).await?;
        let gh = gh::observe_gh(self.gh_search_path().as_deref()).await;
        Ok(CodeCloneDefaults {
            parent_dir,
            gh_found: gh.found,
            gh_authenticated: gh.authenticated,
            gh_remediation: gh.remediation,
        })
    }

    pub(crate) fn get_clone_job(&self, id: Uuid) -> Result<CodeCloneJobSnapshot, ServerError> {
        self.clone_jobs
            .snapshot(id)
            .ok_or_else(|| ServerError::not_found(format!("clone job {id} not found")))
    }

    /// Validate, remember the parent, return a job id, and spawn the clone.
    pub(crate) async fn start_clone(
        self: &std::sync::Arc<Self>,
        owner: &OwnerId,
        request: CloneRequest,
    ) -> Result<CodeCloneJobSnapshot, ServerError> {
        let parent = PathBuf::from(request.parent_dir.trim());
        validate_parent_dir(&parent).await?;
        let source = resolve_clone_source(&request, self.gh_search_path()).await?;
        let name = request
            .name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| infer_clone_name(&source));
        if name.is_empty()
            || name.contains('/')
            || name.contains('\\')
            || name == "."
            || name == ".."
        {
            return Err(ServerError::bad_request_kind(
                "clone_invalid_name",
                "name must be a single path segment",
            ));
        }
        let target = owner_dir(&parent, owner).join(&name);
        if target.exists() {
            return Err(ServerError::conflict_kind(
                "clone_target_exists",
                format!("destination {} already exists", target.display()),
            ));
        }
        write_clone_parent_dir(&*self.db, &parent).await?;

        let id = Uuid::new_v4();
        let job = CloneJob {
            id,
            phase: "starting".into(),
            percent: None,
            done: false,
            error: None,
            repo_id: None,
        };
        self.clone_jobs.insert(job.clone());
        self.publish_clone(owner, &job);

        let runtime = std::sync::Arc::clone(self);
        let owner = owner.clone();
        tokio::spawn(async move {
            runtime.run_clone(&owner, id, source, target).await;
        });
        Ok(job.to_snapshot())
    }

    async fn run_clone(
        self: std::sync::Arc<Self>,
        owner: &OwnerId,
        id: Uuid,
        url: String,
        target: PathBuf,
    ) {
        match clone_into(&url, &target, |phase, percent| {
            self.touch_clone(owner, id, |job| {
                job.phase = phase.to_owned();
                job.percent = Some(percent);
            });
        })
        .await
        {
            Ok(()) => match self
                .register_repo(
                    owner,
                    target,
                    super::runtime::RepoRegistration {
                        cloned_from: Some(redact_clone_url(&url)),
                        ..Default::default()
                    },
                )
                .await
            {
                Ok(repo) => {
                    self.touch_clone(owner, id, |job| {
                        job.phase = "done".into();
                        job.percent = Some(100);
                        job.done = true;
                        job.repo_id = Some(repo.id);
                    });
                }
                Err(error) => {
                    self.fail_clone(owner, id, error.message());
                }
            },
            Err(error) => self.fail_clone(owner, id, error),
        }
    }

    fn touch_clone(&self, owner: &OwnerId, id: Uuid, update: impl FnOnce(&mut CloneJob)) {
        if let Some(job) = self.clone_jobs.apply(id, update) {
            self.publish_clone(owner, &job);
        }
    }

    fn fail_clone(&self, owner: &OwnerId, id: Uuid, error: impl Into<String>) {
        let error = bound_stderr(&error.into());
        self.touch_clone(owner, id, |job| {
            job.phase = "failed".into();
            job.done = true;
            job.error = Some(error);
        });
    }

    fn publish_clone(&self, owner: &OwnerId, job: &CloneJob) {
        self.bus.publish_update(
            owner,
            CodeLiveUpdate::CloneProgress(CloneProgress {
                job: job.id.to_string(),
                phase: job.phase.clone(),
                percent: job.percent,
                done: job.done,
                error: job.error.clone(),
                repo_id: job.repo_id,
            }),
        );
    }

    fn gh_search_path(&self) -> Option<String> {
        #[cfg(test)]
        {
            return self.gh_search_path.lock().expect("gh search path").clone();
        }
        #[cfg(not(test))]
        None
    }
}

async fn resolve_clone_source(
    request: &CloneRequest,
    gh_search_path: Option<String>,
) -> Result<String, ServerError> {
    let url = request
        .url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let github = request
        .github
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match (url, github) {
        (Some(_), Some(_)) => Err(ServerError::bad_request_kind(
            "clone_invalid_source",
            "provide url or github, not both",
        )),
        (None, None) => Err(ServerError::bad_request_kind(
            "clone_invalid_source",
            "provide url or github",
        )),
        (Some(url), None) => Ok(url.to_owned()),
        (None, Some(github)) => {
            if !valid_github_slug(github) {
                return Err(ServerError::bad_request_kind(
                    "clone_invalid_source",
                    "github must be owner/repo",
                ));
            }
            resolve_github_clone_url(github, gh_search_path.as_deref())
                .await
                .map_err(|err| {
                    ServerError::bad_request_kind("clone_github_unresolved", err.to_string())
                })
        }
    }
}

async fn validate_parent_dir(parent: &Path) -> Result<(), ServerError> {
    if parent.as_os_str().is_empty() {
        return Err(ServerError::bad_request_kind(
            "clone_parent_missing",
            "parent_dir must not be empty",
        ));
    }
    let meta = match tokio::fs::metadata(parent).await {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(ServerError::bad_request_kind(
                "clone_parent_missing",
                format!("parent_dir {} does not exist", parent.display()),
            ));
        }
        Err(err) => {
            return Err(ServerError::bad_request_kind(
                "clone_parent_unusable",
                format!("could not read parent_dir {}: {err}", parent.display()),
            ));
        }
    };
    if !meta.is_dir() {
        return Err(ServerError::bad_request_kind(
            "clone_parent_not_dir",
            format!("parent_dir {} is not a directory", parent.display()),
        ));
    }
    Ok(())
}

/// The clone URL with any embedded credentials removed.
///
/// A user may clone `https://token@host/org/repo.git`, and this string is
/// persisted and shown back. Keep the origin, drop the secret: the userinfo
/// segment is the credential, and nothing downstream needs it — the checkout's
/// own remote holds whatever git needs to fetch again.
pub(crate) fn redact_clone_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        // scp-style `git@host:org/repo` carries a username, not a secret.
        return url.to_owned();
    };
    match rest.split_once('@') {
        Some((_userinfo, host)) => format!("{scheme}://{host}"),
        None => url.to_owned(),
    }
}

async fn clone_into(
    url: &str,
    target: &Path,
    mut on_progress: impl FnMut(&str, u8),
) -> Result<(), String> {
    let mut command = Command::new("git");
    command
        .args(["clone", "--progress", "--", url])
        .arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "")
        .env("GCM_INTERACTIVE", "never");
    let mut child = command
        .spawn()
        .map_err(|err| format!("failed to spawn git: {err}"))?;
    let stderr = child.stderr.take().ok_or("git stderr was not piped")?;
    let mut reader = BufReader::new(stderr);
    let mut tail = String::new();
    let run = async {
        loop {
            let Some(line) = read_progress_line(&mut reader).await? else {
                break;
            };
            append_tail(&mut tail, &line);
            if let Some((phase, percent)) = parse_clone_progress_line(&line) {
                on_progress(&phase, percent);
            }
        }
        child
            .wait()
            .await
            .map_err(|err| format!("git clone failed: {err}"))
    };
    let status = timeout(CLONE_TIMEOUT, run)
        .await
        .map_err(|_| "git clone timed out".to_owned())??;
    if status.success() {
        Ok(())
    } else {
        Err(if tail.trim().is_empty() {
            format!("git clone failed (exit {})", status.code().unwrap_or(-1))
        } else {
            tail
        })
    }
}

/// Parse a `git clone --progress` stderr line into a phase name and percent.
pub(crate) fn parse_clone_progress_line(line: &str) -> Option<(String, u8)> {
    let line = line.trim();
    let percent_at = line.find('%')?;
    let before = &line[..percent_at];
    let digits: String = before
        .chars()
        .rev()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    if digits.is_empty() {
        return None;
    }
    let percent: u8 = digits.parse().ok()?;
    let without_remote = line.strip_prefix("remote:").map(str::trim).unwrap_or(line);
    let phase = without_remote
        .split_once(':')
        .map(|(head, _)| head.trim())
        .unwrap_or("cloning")
        .to_ascii_lowercase();
    let phase = if phase.is_empty() {
        "cloning".into()
    } else {
        phase
    };
    Some((phase, percent.min(100)))
}

/// Last path segment of a git URL, minus a trailing `.git`.
pub(crate) fn infer_clone_name(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    let trimmed = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    let segment = trimmed
        .rsplit(['/', ':'])
        .find(|part| !part.is_empty())
        .unwrap_or("repo");
    segment.to_owned()
}

pub(crate) fn valid_github_slug(value: &str) -> bool {
    let Some((owner, repo)) = value.split_once('/') else {
        return false;
    };
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return false;
    }
    slug_part(owner) && slug_part(repo)
}

fn slug_part(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.')
}

async fn read_clone_parent_dir(store: &dyn Store) -> Result<Option<String>, ServerError> {
    Ok(store
        .get_setting(CLONE_PARENT_DIR_SETTING)
        .await?
        .and_then(|value| {
            value
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned)
        }))
}

async fn write_clone_parent_dir(store: &dyn Store, parent: &Path) -> Result<(), ServerError> {
    store
        .set_setting(
            CLONE_PARENT_DIR_SETTING,
            &serde_json::json!(parent.display().to_string()),
        )
        .await?;
    Ok(())
}

async fn read_progress_line<R: AsyncReadExt + Unpin>(
    reader: &mut BufReader<R>,
) -> Result<Option<String>, String> {
    let mut buf = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        match reader.read(&mut byte).await {
            Ok(0) => {
                if buf.is_empty() {
                    return Ok(None);
                }
                break;
            }
            Ok(_) => {
                if byte[0] == b'\n' || byte[0] == b'\r' {
                    if buf.is_empty() {
                        continue;
                    }
                    break;
                }
                buf.push(byte[0]);
            }
            Err(err) => return Err(format!("git clone stderr: {err}")),
        }
    }
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}

fn append_tail(tail: &mut String, line: &str) {
    if !tail.is_empty() {
        tail.push('\n');
    }
    tail.push_str(line);
    *tail = bound_stderr(tail);
}

fn bound_stderr(text: &str) -> String {
    let mut owned = text.to_owned();
    if owned.chars().count() > MAX_STDERR_CHARS {
        owned = owned
            .chars()
            .rev()
            .take(MAX_STDERR_CHARS)
            .collect::<String>();
        owned = owned.chars().rev().collect();
    }
    owned
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_parser_reads_git_percent_lines() {
        assert_eq!(
            parse_clone_progress_line("Receiving objects:  45% (45/100)"),
            Some(("receiving objects".into(), 45))
        );
        assert_eq!(
            parse_clone_progress_line("remote: Compressing objects: 100% (50/50), done."),
            Some(("compressing objects".into(), 100))
        );
        assert_eq!(parse_clone_progress_line("Cloning into 'foo'..."), None);
    }

    #[test]
    fn clone_name_strips_git_suffix_and_path() {
        assert_eq!(infer_clone_name("https://github.com/acme/demo.git"), "demo");
        assert_eq!(infer_clone_name("git@github.com:acme/demo.git"), "demo");
        assert_eq!(infer_clone_name("/tmp/origin.git"), "origin");
    }

    #[test]
    fn github_slug_requires_owner_repo() {
        assert!(valid_github_slug("acme/demo"));
        assert!(valid_github_slug("acme.inc/my_repo-1"));
        assert!(!valid_github_slug("acme"));
        assert!(!valid_github_slug("acme/demo/extra"));
        assert!(!valid_github_slug("acme/de mo"));
    }

    /// A clone URL is persisted and shown back, so a credential embedded in it
    /// must not be what gets stored.
    #[test]
    fn a_clone_url_keeps_its_origin_and_loses_its_credential() {
        assert_eq!(
            redact_clone_url("https://ghp_secret@github.com/acme/demo.git"),
            "https://github.com/acme/demo.git"
        );
        assert_eq!(
            redact_clone_url("https://user:token@git.example.com/acme/demo.git"),
            "https://git.example.com/acme/demo.git"
        );
        assert_eq!(
            redact_clone_url("https://github.com/acme/demo.git"),
            "https://github.com/acme/demo.git"
        );
        // scp-style carries a username, not a secret.
        assert_eq!(
            redact_clone_url("git@github.com:acme/demo.git"),
            "git@github.com:acme/demo.git"
        );
    }
}
