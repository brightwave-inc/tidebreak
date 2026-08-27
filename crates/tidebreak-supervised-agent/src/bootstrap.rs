//! Everything the pod does before the first poll.
//!
//! The agent is the pod's only command, so the setup a hosted runtime would
//! do around it falls to the agent itself: prepare outbound trust, clone the
//! declared repositories or prepare a bare workspace, and pre-create the
//! completion latch directory. Each step emits the lifecycle event the
//! supervising environment expects, collected here and delivered ahead of
//! `supervisor_started` so the stream reads in the order the work happened.
//!
//! Bootstrap is blocking and runs before the async loop: nothing here is
//! worth overlapping, and a failure must reach the pod log before anything
//! else runs.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::completion::completion_latch_path;
use crate::inputs::{Inputs, Repository};
use crate::trust::{self, Trust, TrustOptions};

/// One collected lifecycle event: kind and payload.
pub type Event = (String, serde_json::Value);

/// A cloned repository's place in the workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClonedRepository {
    /// Clone directory, under the workspace.
    pub directory: PathBuf,
    /// Position in the declared order, zero-based.
    pub position: usize,
}

/// The prepared pod: trust, working directory, clones, and the events that
/// describe how it got there.
#[derive(Debug)]
pub struct Bootstrap {
    /// Outbound trust every spawned child carries.
    pub trust: Trust,
    /// Where the engine runs: the first clone, or the workspace itself.
    pub workdir: PathBuf,
    /// Every clone, in declared order.
    pub clones: Vec<ClonedRepository>,
    /// Lifecycle events, in the order the work happened.
    pub events: Vec<Event>,
}

/// Bootstrap failed; the stage names which step.
#[derive(Debug, thiserror::Error)]
#[error("{stage}: {message}")]
pub struct BootstrapError {
    /// Which step failed: `trust_prepare_failed`, `repository_clone_failed`,
    /// or `workspace_prepare_failed`.
    pub stage: &'static str,
    /// What went wrong, with enough context to act on from the pod log.
    pub message: String,
}

/// Prepares the pod and collects the lifecycle events describing it.
pub fn run(
    inputs: &Inputs,
    trust_options: &TrustOptions,
    effective_reasoning_effort: Option<&str>,
) -> Result<Bootstrap, BootstrapError> {
    let mut events: Vec<Event> = Vec::new();
    let mut push = |kind: &str, payload: serde_json::Value| {
        events.push((kind.to_owned(), payload));
    };

    push(
        "bootstrap_started",
        serde_json::json!({
            "harness": "custom",
            "repository": inputs.repositories.first().map(|repository| &repository.url),
            "workspace": inputs.workspace.display().to_string(),
        }),
    );

    let trust = trust::prepare(trust_options).map_err(|error| BootstrapError {
        stage: "trust_prepare_failed",
        message: error.message,
    })?;
    push(
        "trust_prepared",
        serde_json::json!({
            "bundle": trust.bundle.display().to_string(),
            "merged_system_roots": trust.merged_system_roots,
        }),
    );

    push(
        "harness_configured",
        serde_json::json!({
            "harness": "custom",
            "model": inputs.model,
            "requested_reasoning_effort": inputs.reasoning_effort,
            "effective_reasoning_effort": effective_reasoning_effort,
            "scope": "user",
        }),
    );

    let mut clones = Vec::new();
    let workdir = if inputs.repositories.is_empty() {
        std::fs::create_dir_all(&inputs.workspace).map_err(|error| BootstrapError {
            stage: "workspace_prepare_failed",
            message: format!(
                "the workspace {} could not be created: {error}",
                inputs.workspace.display()
            ),
        })?;
        push(
            "workspace_prepared",
            serde_json::json!({ "directory": inputs.workspace.display().to_string() }),
        );
        inputs.workspace.clone()
    } else {
        let directories = assign_repository_directories(&inputs.repositories);
        for (position, (repository, directory)) in
            inputs.repositories.iter().zip(&directories).enumerate()
        {
            let target = clone_repository(&inputs.workspace, repository, directory, &trust)
                .map_err(|message| BootstrapError {
                    stage: "repository_clone_failed",
                    message,
                })?;
            push(
                "repository_cloned",
                serde_json::json!({
                    "repository": repository.url,
                    "ref": repository.repository_ref,
                    "directory": target.display().to_string(),
                    "position": position,
                }),
            );
            clones.push(ClonedRepository {
                directory: target,
                position,
            });
        }
        clones[0].directory.clone()
    };

    // The latch directory is best-effort: a task that cannot write its latch
    // still runs, but the operator should know the channel is missing.
    let latch = completion_latch_path(&inputs.workspace);
    if let Some(parent) = latch.parent() {
        if let Err(error) = std::fs::create_dir_all(parent) {
            push(
                "completion_latch_unavailable",
                serde_json::json!({
                    "path": crate::completion::COMPLETION_LATCH_PATH,
                    "error": error.to_string(),
                }),
            );
        }
    }

    Ok(Bootstrap {
        trust,
        workdir,
        clones,
        events,
    })
}

/// Derives a clone directory name from a repository URL.
///
/// The last path segment, without any `.git` suffix, filtered to filename-safe
/// characters. An empty result falls back to `repository`.
#[must_use]
pub fn repository_directory(url: &str) -> String {
    let trimmed = url.trim_end_matches('/');
    let path = match trimmed.split_once("://") {
        Some((_, rest)) => rest.split_once('/').map_or("", |(_, path)| path),
        None => trimmed.split_once(':').map_or(trimmed, |(_, path)| path),
    };
    let segment = path
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .trim_end_matches(".git");
    let directory: String = segment
        .chars()
        .filter(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
        .collect();
    let directory = directory.trim_matches('.').to_owned();
    if directory.is_empty() {
        "repository".to_owned()
    } else {
        directory
    }
}

/// Assigns each declared repository a distinct directory name.
///
/// Two repositories with the same basename get numbered suffixes in declared
/// order: the first keeps the base, later ones take the first free
/// `<base>-2`, `<base>-3`, and so on.
#[must_use]
pub fn assign_repository_directories(repositories: &[Repository]) -> Vec<String> {
    let mut taken: Vec<String> = Vec::new();
    for repository in repositories {
        let base = repository_directory(&repository.url);
        let mut candidate = base.clone();
        let mut counter = 2;
        while taken.contains(&candidate) {
            candidate = format!("{base}-{counter}");
            counter += 1;
        }
        taken.push(candidate);
    }
    taken
}

/// Clones one repository into the workspace and checks out its declared ref.
fn clone_repository(
    workspace: &Path,
    repository: &Repository,
    directory: &str,
    trust: &Trust,
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(workspace).map_err(|error| {
        format!(
            "the workspace {} could not be created: {error}",
            workspace.display()
        )
    })?;
    let target = workspace.join(directory);
    run_git(
        workspace,
        trust,
        &[
            "-c",
            "credential.helper=",
            "clone",
            "--",
            &repository.url,
            &target.display().to_string(),
        ],
    )
    .map_err(|error| format!("cloning {} failed: {error}", repository.url))?;
    if let Some(reference) = &repository.repository_ref {
        if run_git(&target, trust, &["checkout", reference]).is_err() {
            // A commit that is not on the cloned branches — a PR head, most
            // often — needs an explicit fetch before it can be checked out.
            run_git(
                &target,
                trust,
                &[
                    "-c",
                    "credential.helper=",
                    "fetch",
                    "origin",
                    "--",
                    reference,
                ],
            )
            .map_err(|error| {
                format!(
                    "fetching {} from {} failed: {error}",
                    reference, repository.url
                )
            })?;
            run_git(&target, trust, &["checkout", reference]).map_err(|error| {
                format!(
                    "checking out {} in {} failed: {error}",
                    reference,
                    target.display()
                )
            })?;
        }
    }
    Ok(target)
}

/// Runs one git command with the trust environment, output inherited so it
/// lands in the pod log.
fn run_git(directory: &Path, trust: &Trust, arguments: &[&str]) -> Result<(), String> {
    let mut command = Command::new("git");
    command
        .args(arguments)
        .current_dir(directory)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null());
    for (name, value) in trust.environment() {
        command.env(name, value);
    }
    let status = command
        .status()
        .map_err(|error| format!("git could not be started: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("git exited with {status}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inputs::{resolve, RawInputs};
    use std::time::Duration;

    #[test]
    fn directory_names_come_from_the_last_path_segment() {
        assert_eq!(
            repository_directory("https://example.com/org/tidebreak.git"),
            "tidebreak"
        );
        assert_eq!(
            repository_directory("https://example.com/org/tidebreak/"),
            "tidebreak"
        );
        assert_eq!(
            repository_directory("git@example.com:org/tidebreak.git"),
            "tidebreak"
        );
        assert_eq!(repository_directory("https://example.com/"), "repository");
        assert_eq!(
            repository_directory("https://example.com/org/.."),
            "repository"
        );
    }

    #[test]
    fn colliding_names_take_numbered_suffixes_in_declared_order() {
        let repositories = vec![
            Repository {
                url: "https://example.com/a/app.git".to_owned(),
                repository_ref: None,
            },
            Repository {
                url: "https://example.com/b/app.git".to_owned(),
                repository_ref: None,
            },
            Repository {
                url: "https://example.com/c/app.git".to_owned(),
                repository_ref: None,
            },
        ];
        assert_eq!(
            assign_repository_directories(&repositories),
            ["app", "app-2", "app-3"]
        );
    }

    fn trust_options(root: &Path) -> TrustOptions {
        let certificate = root.join("ca.crt");
        std::fs::write(&certificate, "CA\n").unwrap();
        TrustOptions {
            certificate,
            timeout: Duration::ZERO,
            baseline: None,
            bundle: root.join("bundle.pem"),
        }
    }

    fn inputs(workspace: &Path) -> Inputs {
        resolve(RawInputs {
            task: Some("do the thing".to_owned()),
            workspace: Some(workspace.display().to_string()),
            ..RawInputs::default()
        })
        .unwrap()
    }

    /// Builds a local git remote with one commit on `main` and a second
    /// commit on a `feature` branch.
    fn fixture_remote(root: &Path) -> PathBuf {
        let remote = root.join("remote");
        std::fs::create_dir_all(&remote).unwrap();
        let git = |arguments: &[&str]| {
            let status = Command::new("git")
                .args(arguments)
                .current_dir(&remote)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "git {arguments:?} failed");
        };
        git(&["init", "--initial-branch=main"]);
        std::fs::write(remote.join("README.md"), "hello\n").unwrap();
        git(&["add", "-A"]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "-m",
            "first",
        ]);
        git(&["branch", "feature"]);
        remote
    }

    #[test]
    fn a_research_run_prepares_a_bare_workspace() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let bootstrap = run(&inputs(&workspace), &trust_options(root.path()), None).unwrap();
        assert!(workspace.is_dir());
        assert_eq!(bootstrap.workdir, workspace);
        assert!(bootstrap.clones.is_empty());
        // The latch directory is pre-created so the task can just write the
        // file.
        assert!(workspace.join(".model-gateway").is_dir());
        let kinds: Vec<&str> = bootstrap
            .events
            .iter()
            .map(|(kind, _)| kind.as_str())
            .collect();
        assert_eq!(
            kinds,
            [
                "bootstrap_started",
                "trust_prepared",
                "harness_configured",
                "workspace_prepared",
            ]
        );
        let started = &bootstrap.events[0].1;
        assert_eq!(started["repository"], serde_json::Value::Null);
    }

    #[test]
    fn a_declared_repository_is_cloned_and_its_ref_checked_out() {
        let root = tempfile::tempdir().unwrap();
        let remote = fixture_remote(root.path());
        let workspace = root.path().join("workspace");
        let mut inputs = inputs(&workspace);
        inputs.repositories = vec![Repository {
            url: remote.display().to_string(),
            repository_ref: Some("feature".to_owned()),
        }];
        let bootstrap = run(&inputs, &trust_options(root.path()), None).unwrap();
        assert_eq!(bootstrap.workdir, workspace.join("remote"));
        assert!(bootstrap.workdir.join("README.md").is_file());
        let head = Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(&bootstrap.workdir)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&head.stdout).trim(), "feature");
        let cloned = bootstrap
            .events
            .iter()
            .find(|(kind, _)| kind == "repository_cloned")
            .unwrap();
        assert_eq!(cloned.1["position"], 0);
        assert_eq!(cloned.1["ref"], "feature");
    }

    #[test]
    fn a_failed_clone_names_its_stage() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let mut inputs = inputs(&workspace);
        inputs.repositories = vec![Repository {
            url: root.path().join("missing").display().to_string(),
            repository_ref: None,
        }];
        let error = run(&inputs, &trust_options(root.path()), None).unwrap_err();
        assert_eq!(error.stage, "repository_clone_failed");
        assert!(error.message.contains("missing"));
    }
}
