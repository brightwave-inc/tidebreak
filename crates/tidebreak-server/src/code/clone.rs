//! Bounded `git clone` jobs for adding a remote repository.
//!
//! The user's own `git` binary does the work. Arguments are an argv array,
//! never a shell string. Credential helpers may authenticate; the process
//! never prompts and never stores secrets.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio::time::timeout;
use uuid::Uuid;

use tidebreak_core::{OwnerId, RepoId, Store};

use super::bus::{CloneProgress, CodeLiveUpdate};
use super::gh::{self, resolve_github_clone_url};
use super::runtime::CodeRuntime;
use crate::error::ServerError;
use crate::obo_gateway::{GitCredential, GitForgeAttribution, GitForgeError, GitForgeIdentity};
use crate::routes::code::{
    CodeCloneDefaults, CodeCloneJobSnapshot, CodeGithubRepositories, CodeRepoSource,
    CodeRepoSources,
};

const CLONE_TIMEOUT: Duration = Duration::from_secs(900);
/// Long enough for a cold binary to answer, short enough that a machine
/// without git reports so rather than stalling the dialog that asked.
const GIT_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_STDERR_CHARS: usize = 4_096;
const COMPLETED_JOB_RETENTION: Duration = Duration::from_secs(30 * 60);
const MAX_COMPLETED_JOBS: usize = 256;
pub(crate) const CLONE_PARENT_DIR_SETTING: &str = "code_clone_parent_dir";

/// In-memory clone jobs for this process. Not journaled; a restart drops them.
#[derive(Debug, Default)]
pub(crate) struct CloneJobs {
    jobs: Mutex<std::collections::HashMap<Uuid, CloneJob>>,
}

#[derive(Debug, Clone)]
struct CloneJob {
    id: Uuid,
    owner: OwnerId,
    phase: String,
    percent: Option<u8>,
    done: bool,
    error: Option<String>,
    repo_id: Option<RepoId>,
    finished_at: Option<Instant>,
}

impl CloneJobs {
    fn snapshot(&self, owner: &OwnerId, id: Uuid) -> Option<CodeCloneJobSnapshot> {
        let mut jobs = self.jobs.lock().expect("clone jobs");
        prune_completed_jobs(&mut jobs, Instant::now());
        jobs.get(&id)
            .filter(|job| &job.owner == owner)
            .map(CloneJob::to_snapshot)
    }

    fn insert(&self, job: CloneJob) {
        let mut jobs = self.jobs.lock().expect("clone jobs");
        jobs.insert(job.id, job);
        prune_completed_jobs(&mut jobs, Instant::now());
    }

    fn apply(
        &self,
        owner: &OwnerId,
        id: Uuid,
        update: impl FnOnce(&mut CloneJob),
    ) -> Option<CloneJob> {
        let now = Instant::now();
        let mut jobs = self.jobs.lock().expect("clone jobs");
        prune_completed_jobs(&mut jobs, now);
        let updated = {
            let job = jobs.get_mut(&id)?;
            if &job.owner != owner {
                return None;
            }
            update(job);
            if job.done && job.finished_at.is_none() {
                job.finished_at = Some(now);
            }
            job.clone()
        };
        prune_completed_jobs(&mut jobs, now);
        Some(updated)
    }
}

fn prune_completed_jobs(jobs: &mut std::collections::HashMap<Uuid, CloneJob>, now: Instant) {
    jobs.retain(|_, job| {
        !job.done
            || job.finished_at.is_none_or(|finished_at| {
                now.saturating_duration_since(finished_at) <= COMPLETED_JOB_RETENTION
            })
    });

    let mut completed = jobs
        .iter()
        .filter_map(|(id, job)| job.finished_at.map(|finished_at| (*id, finished_at)))
        .collect::<Vec<_>>();
    if completed.len() <= MAX_COMPLETED_JOBS {
        return;
    }
    completed.sort_unstable_by_key(|(_, finished_at)| *finished_at);
    let excess = completed.len() - MAX_COMPLETED_JOBS;
    for (id, _) in completed.into_iter().take(excess) {
        jobs.remove(&id);
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
    /// Where to put the checkout. Optional: a machine whose operator
    /// configured a destination places clones itself, so a caller who cannot
    /// see the machine's filesystem — anyone attached to it — names nothing.
    pub parent_dir: Option<String>,
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

    /// What this machine can add a repository from.
    ///
    /// On a machine with its own credentials, every answer is about the
    /// machine — a client pairs it with what it knows about itself and
    /// renders the intersection. On a gateway-authenticated hosted machine
    /// the `github` answer is about the caller too: it reflects whether the
    /// deployment's gateway would lend *this* caller the forge's App
    /// identity (decision 63), probed live rather than assumed.
    pub(crate) async fn repo_sources(
        &self,
        owner: &OwnerId,
    ) -> Result<CodeRepoSources, ServerError> {
        let git = git_available().await;
        let no_git = || {
            Some(
                "This machine has no git. Install it there, or use a machine that has it."
                    .to_owned(),
            )
        };
        let github = if let Some(lender) = self.git_credentials() {
            let outcome = lender.git_forge_identity(owner).await;
            hosted_github_source(git, no_git(), outcome)
        } else {
            // GitHub needs exactly what a git URL needs. Without a `gh`
            // credential `resolve_github_clone_url` falls back to the public
            // HTTPS URL, so `owner/repo` still clones anything public —
            // hiding the form would take away a path that works. What `gh`
            // buys is private repositories, so its absence rides along as a
            // hint on an available source rather than as unavailability.
            let gh = gh::observe_gh(self.gh_search_path().as_deref()).await;
            let github_credential = gh.found && gh.authenticated == Some(true);
            CodeRepoSource {
                kind: "github".to_owned(),
                available: git,
                remediation: if !git {
                    no_git()
                } else if github_credential {
                    None
                } else {
                    Some(gh.remediation.clone())
                },
            }
        };
        let sources = vec![
            // Always offered: a machine can always register a checkout that
            // is already on its own disk. Whether the caller can name one is
            // the caller's question, not this one.
            CodeRepoSource {
                kind: "local".to_owned(),
                available: true,
                remediation: None,
            },
            CodeRepoSource {
                kind: "git_url".to_owned(),
                available: git,
                remediation: if git { None } else { no_git() },
            },
            github,
        ];
        Ok(CodeRepoSources {
            sources,
            chooses_destination: self.chooses_clone_destination().await?,
        })
    }

    /// Whether this machine places clones itself: a stored destination, or
    /// the embedding's default (decision 70).
    async fn chooses_clone_destination(&self) -> Result<bool, ServerError> {
        if self.clone_parent_default.is_some() {
            return Ok(true);
        }
        Ok(read_clone_parent_dir(&*self.db).await?.is_some())
    }

    /// The parent directory a clone lands under: what the caller asked for,
    /// the destination the machine already remembers, or the embedding's
    /// default.
    ///
    /// Preferring the caller keeps a desktop working on its own machine
    /// exactly as before. Falling back to the setting, then the default, is
    /// what lets a caller who cannot see the filesystem clone at all.
    async fn clone_parent(&self, requested: Option<&str>) -> Result<PathBuf, ServerError> {
        if let Some(value) = requested.map(str::trim).filter(|value| !value.is_empty()) {
            return Ok(PathBuf::from(value));
        }
        if let Some(configured) = read_clone_parent_dir(&*self.db).await? {
            return Ok(PathBuf::from(configured));
        }
        if let Some(default) = &self.clone_parent_default {
            ensure_parent_dir(default).await?;
            return Ok(default.clone());
        }
        Err(ServerError::bad_request_kind(
            "clone_parent_missing",
            "this machine has no clone destination configured, so the request must name one",
        ))
    }

    /// Repositories this caller can clone from GitHub, for the add-repository
    /// picker (decision 70).
    ///
    /// A hosted machine asks the gateway; a machine with no lender answers
    /// an empty list so the typed `owner/repo` field stays the path. A
    /// lender error is a failed list, not an empty one: the dialog keeps
    /// type-in and says the suggestions did not load.
    pub(crate) async fn list_github_repositories(
        &self,
        owner: &OwnerId,
    ) -> Result<CodeGithubRepositories, ServerError> {
        let Some(lender) = self.git_credentials() else {
            return Ok(CodeGithubRepositories {
                repositories: Vec::new(),
            });
        };
        match lender.list_repositories(owner).await {
            Ok(repositories) => Ok(CodeGithubRepositories {
                repositories: repositories
                    .into_iter()
                    .map(|repository| crate::routes::code::CodeGithubRepository {
                        full_name: repository.full_name,
                        private: repository.private,
                        description: repository.description,
                    })
                    .collect(),
            }),
            Err(error) => Err(ServerError::unprocessable_kind(
                "git_forge_refused",
                git_forge_refusal_message(&error),
            )),
        }
    }

    pub(crate) fn get_clone_job(
        &self,
        owner: &OwnerId,
        id: Uuid,
    ) -> Result<CodeCloneJobSnapshot, ServerError> {
        self.clone_jobs
            .snapshot(owner, id)
            .ok_or_else(|| ServerError::not_found(format!("clone job {id} not found")))
    }

    /// Validate, remember the parent, return a job id, and spawn the clone.
    pub(crate) async fn start_clone(
        self: &std::sync::Arc<Self>,
        owner: &OwnerId,
        request: CloneRequest,
    ) -> Result<CodeCloneJobSnapshot, ServerError> {
        let parent = self.clone_parent(request.parent_dir.as_deref()).await?;
        validate_parent_dir(&parent).await?;
        let source = resolve_clone_source(
            &request,
            self.gh_search_path(),
            self.git_credentials().is_some(),
        )
        .await?;
        let name = request
            .name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| infer_clone_name(&source.url));
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
            owner: owner.clone(),
            phase: "starting".into(),
            percent: None,
            done: false,
            error: None,
            repo_id: None,
            finished_at: None,
        };
        self.clone_jobs.insert(job.clone());
        self.publish_clone(&job);

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
        source: CloneSource,
        target: PathBuf,
    ) {
        // Borrowed at the moment of use and dropped with this job: a hosted
        // machine clones a GitHub repository with a dying, repository-scoped
        // credential the gateway mints for this caller (decision 63).
        let credential = match (source.github_slug.as_deref(), self.git_credentials()) {
            (Some(slug), Some(lender)) => match lender.git_credential(owner, slug).await {
                Ok(credential) => Some(credential),
                Err(refusal) => {
                    self.fail_clone(owner, id, git_forge_refusal_message(&refusal));
                    return;
                }
            },
            _ => None,
        };
        match clone_into(
            &source.url,
            &target,
            credential.as_ref(),
            |phase, percent| {
                self.touch_clone(owner, id, |job| {
                    job.phase = phase.to_owned();
                    job.percent = Some(percent);
                });
            },
        )
        .await
        {
            Ok(()) => match self
                .register_repo(
                    owner,
                    target,
                    super::runtime::RepoRegistration {
                        cloned_from: Some(redact_clone_url(&source.url)),
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
        if let Some(job) = self.clone_jobs.apply(owner, id, update) {
            self.publish_clone(&job);
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

    fn publish_clone(&self, job: &CloneJob) {
        self.bus.publish_update(
            &job.owner,
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

/// Where a clone reads from, plus the GitHub slug when the caller named one.
///
/// The slug survives resolution so a hosted machine can borrow a credential
/// scoped to exactly that repository at clone time (decision 63).
struct CloneSource {
    url: String,
    github_slug: Option<String>,
}

async fn resolve_clone_source(
    request: &CloneRequest,
    gh_search_path: Option<String>,
    gateway_credentials: bool,
) -> Result<CloneSource, ServerError> {
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
        (Some(url), None) => Ok(CloneSource {
            url: url.to_owned(),
            github_slug: None,
        }),
        (None, Some(github)) => {
            if !valid_github_slug(github) {
                return Err(ServerError::bad_request_kind(
                    "clone_invalid_source",
                    "github must be owner/repo",
                ));
            }
            if gateway_credentials {
                // A hosted machine never consults `gh` — there is none, and
                // nothing to observe if there were. The URL shape is fixed,
                // and the credential arrives per operation at clone time.
                return Ok(CloneSource {
                    url: format!("https://github.com/{github}.git"),
                    github_slug: Some(github.to_owned()),
                });
            }
            resolve_github_clone_url(github, gh_search_path.as_deref())
                .await
                .map(|url| CloneSource {
                    url,
                    github_slug: None,
                })
                .map_err(|err| {
                    ServerError::bad_request_kind("clone_github_unresolved", err.to_string())
                })
        }
    }
}

/// The `github` repo source as a gateway-authenticated hosted machine offers
/// it to one caller (decision 63).
///
/// An identity means the source works and the remediation slot carries the
/// attribution sentence — the one place the add-repository dialog states out
/// loud that work lands as the App. Every named refusal keeps the source
/// hidden and says why, so a deployment with no forge reads as "not
/// offered", never as an error.
fn hosted_github_source(
    git: bool,
    no_git: Option<String>,
    outcome: Result<GitForgeIdentity, GitForgeError>,
) -> CodeRepoSource {
    if !git {
        return CodeRepoSource {
            kind: "github".to_owned(),
            available: false,
            remediation: no_git,
        };
    }
    match outcome {
        Ok(identity) => CodeRepoSource {
            kind: "github".to_owned(),
            available: true,
            remediation: Some(hosted_attribution_sentence(&identity)),
        },
        Err(refusal) => CodeRepoSource {
            kind: "github".to_owned(),
            available: false,
            remediation: Some(git_forge_refusal_message(&refusal)),
        },
    }
}

/// The sentence a hosted machine states its git identity with.
///
/// Issue #2510's contract, extended by decision 65: the add-repository
/// dialog says out loud whose account work lands as — the deployment's App
/// (decision 63), or the caller's own once they have connected it.
pub(crate) fn hosted_attribution_sentence(identity: &GitForgeIdentity) -> String {
    match &identity.attribution {
        GitForgeAttribution::Person { login, .. } => format!(
            "Clones and pushes use your own GitHub account: work lands as {login}."
        ),
        GitForgeAttribution::Bot {
            bot_login: Some(bot_login),
        } => format!(
            "Clones and pushes use this deployment's GitHub App: work lands as {bot_login}, not as your GitHub account."
        ),
        GitForgeAttribution::Bot { bot_login: None } => format!(
            "Clones and pushes use this deployment's GitHub App ({}): work lands as the App's bot account, not as your GitHub account.",
            identity.app_name
        ),
    }
}

/// One user-facing sentence for a git-forge refusal, phrased for the
/// operation or offer it stopped.
pub(crate) fn git_forge_refusal_message(refusal: &GitForgeError) -> String {
    match refusal {
        GitForgeError::SignInRequired(detail) | GitForgeError::Unavailable(detail) => {
            detail.clone()
        }
        GitForgeError::NoGitForge => "This deployment has no git forge configured, so GitHub \
                                      repositories are not offered. An administrator can register \
                                      an installation-mode git forge app on the Model Gateway."
            .to_owned(),
        GitForgeError::AmbiguousGitForge => "This deployment's gateway has more than one git \
                                             forge, and this machine serves exactly one. An \
                                             administrator can disable the extras."
            .to_owned(),
        GitForgeError::ConnectModeForge => "This deployment's git forge identifies each person \
                                            individually, and its gateway does not lend personal \
                                            credentials to this machine. An administrator can \
                                            update the Model Gateway to one that serves personal \
                                            git credentials."
            .to_owned(),
        GitForgeError::NotConnected { connect_url } => match connect_url {
            Some(url) => format!(
                "To use GitHub as yourself here, connect your GitHub account at the Model \
                 Gateway: {url}"
            ),
            None => "To use GitHub as yourself here, connect your GitHub account at the Model \
                     Gateway."
                .to_owned(),
        },
        GitForgeError::ForgeAppNotInstalled => "This deployment's git forge app has no approved \
                                                GitHub App installation yet. An administrator can \
                                                finish installing it on GitHub."
            .to_owned(),
        GitForgeError::RepositoryNotInstalled => "The deployment's GitHub App installation does \
                                                  not cover this repository. An administrator can \
                                                  add it to the installation on GitHub."
            .to_owned(),
    }
}

/// Whether this machine can spawn `git` at all.
///
/// Probed by running it rather than by walking `PATH`. What matters is that
/// the clone below can spawn it, and a `PATH` entry that is not executable —
/// or a Windows extension a walk missed — would answer a different question
/// than the one asked.
pub(crate) async fn git_available() -> bool {
    matches!(
        timeout(
            GIT_PROBE_TIMEOUT,
            Command::new("git")
                .arg("--version")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .status(),
        )
        .await,
        Ok(Ok(status)) if status.success()
    )
}

async fn ensure_parent_dir(parent: &Path) -> Result<(), ServerError> {
    if let Err(err) = tokio::fs::create_dir_all(parent).await {
        return Err(ServerError::bad_request_kind(
            "clone_parent_unusable",
            format!("could not create parent_dir {}: {err}", parent.display()),
        ));
    }
    validate_parent_dir(parent).await
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
    credential: Option<&GitCredential>,
    mut on_progress: impl FnMut(&str, u8),
) -> Result<(), String> {
    let mut command = Command::new("git");
    // The credential rides the environment into a one-shot helper, never the
    // URL: the URL is persisted and shown back, and the helper reset keeps
    // any configured helper from storing the dying token (decision 63).
    if let Some(credential) = credential {
        command.args(gh::GIT_CREDENTIAL_CONFIG_ARGS);
        command
            .env(gh::GIT_CREDENTIAL_USERNAME_ENV, &credential.username)
            .env(gh::GIT_CREDENTIAL_SECRET_ENV, &credential.secret)
            .env(gh::GIT_CREDENTIAL_HOST_ENV, gh::GIT_CREDENTIAL_FORGE_HOST);
    }
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

    fn job(owner: &OwnerId, done: bool, finished_at: Option<Instant>) -> CloneJob {
        CloneJob {
            id: Uuid::new_v4(),
            owner: owner.clone(),
            phase: if done { "done" } else { "starting" }.to_owned(),
            percent: done.then_some(100),
            done,
            error: None,
            repo_id: None,
            finished_at,
        }
    }

    #[test]
    fn clone_jobs_are_visible_only_to_their_owner() {
        let jobs = CloneJobs::default();
        let alice = OwnerId::new("user:alice").unwrap();
        let bob = OwnerId::new("user:bob").unwrap();
        let job = job(&alice, false, None);
        let id = job.id;
        jobs.insert(job);

        assert!(jobs.snapshot(&alice, id).is_some());
        assert!(jobs.snapshot(&bob, id).is_none());
        assert!(jobs.apply(&bob, id, |_| {}).is_none());
    }

    #[test]
    fn completed_clone_jobs_expire_and_stay_bounded() {
        let jobs = CloneJobs::default();
        let owner = OwnerId::new("user:alice").unwrap();
        let expired = job(
            &owner,
            true,
            Instant::now().checked_sub(COMPLETED_JOB_RETENTION + Duration::from_secs(1)),
        );
        let expired_id = expired.id;
        jobs.insert(expired);
        assert!(jobs.snapshot(&owner, expired_id).is_none());

        for offset in 0..=MAX_COMPLETED_JOBS {
            let finished_at = Instant::now()
                .checked_sub(Duration::from_millis((MAX_COMPLETED_JOBS - offset) as u64))
                .unwrap();
            jobs.insert(job(&owner, true, Some(finished_at)));
        }
        let guard = jobs.jobs.lock().expect("clone jobs");
        assert_eq!(guard.len(), MAX_COMPLETED_JOBS);
        assert!(guard.values().all(|job| job.done));
    }

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
