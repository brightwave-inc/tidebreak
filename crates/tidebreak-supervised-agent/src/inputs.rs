//! The environment contract the supervising sandbox injects.
//!
//! Every input arrives as a `MODEL_GATEWAY_SANDBOX_*` variable on the pod
//! specification. The names are mirrored here rather than shared: the
//! environment publishes them as its integration contract for bring-your-own
//! workloads, and the coupling that matters is the pod specification, not a
//! crate boundary.
//!
//! The surface is treated as unversioned. Every variable the agent can
//! default is defaulted, and an empty value means the same as an absent one,
//! because the environment writes empty strings for fields the spawn did not
//! declare. Only the task is genuinely required — an agent with no task has
//! nothing to do and fails loudly, naming the variable, so the operator
//! reading a terminated pod learns which field of the pod specification is
//! wrong.

use std::path::PathBuf;
use std::time::Duration;

use tidebreak_core::HarnessKind;

use crate::EXIT_MISSING_INPUT;

/// The task handed to the agent. Required.
pub const TASK_VARIABLE: &str = "MODEL_GATEWAY_SANDBOX_TASK";
/// Control endpoint as `host:port`; `http://` is assumed when no scheme.
pub const SUPERVISOR_ENDPOINT_VARIABLE: &str = "MODEL_GATEWAY_SANDBOX_SUPERVISOR_ENDPOINT";
/// Workspace directory the engine runs in. Defaults to the working directory.
pub const WORKSPACE_VARIABLE: &str = "MODEL_GATEWAY_SANDBOX_WORKSPACE";
/// Continuation policy: `goal` (default) or `turn`.
pub const MODE_VARIABLE: &str = "MODEL_GATEWAY_SANDBOX_MODE";
/// Turn budget including the spawn-task turn. Empty means uncapped.
pub const MAX_TURNS_VARIABLE: &str = "MODEL_GATEWAY_SANDBOX_MAX_TURNS";
/// Turn number this incarnation starts at. Defaults to 1.
pub const STARTING_TURN_VARIABLE: &str = "MODEL_GATEWAY_SANDBOX_STARTING_TURN";
/// Model route the engine should be driven with. Empty means engine default.
pub const MODEL_VARIABLE: &str = "MODEL_GATEWAY_SANDBOX_MODEL";
/// Requested reasoning effort. Empty means the engine and model default.
pub const REASONING_EFFORT_VARIABLE: &str = "MODEL_GATEWAY_SANDBOX_REASONING_EFFORT";
/// Primary repository HTTPS URL. Empty means a research sandbox: no clone.
pub const REPOSITORY_URL_VARIABLE: &str = "MODEL_GATEWAY_SANDBOX_REPOSITORY_URL";
/// Branch or commit to check out. Empty means the remote's default branch.
pub const REPOSITORY_REF_VARIABLE: &str = "MODEL_GATEWAY_SANDBOX_REPOSITORY_REF";
/// JSON array of `{url, repository_ref}` for every declared repository.
pub const REPOSITORIES_VARIABLE: &str = "MODEL_GATEWAY_SANDBOX_REPOSITORIES";
/// Forge push posture: `denied`, `allowed`, or absent for research.
pub const FORGE_PUSH_VARIABLE: &str = "MODEL_GATEWAY_SANDBOX_FORGE_PUSH";
/// Sandbox identifier, when the environment names one.
pub const SANDBOX_ID_VARIABLE: &str = "MODEL_GATEWAY_SANDBOX_ID";
/// Incarnation counter for reincarnation-aware naming.
pub const INCARNATION_VARIABLE: &str = "MODEL_GATEWAY_SANDBOX_INCARNATION";
/// Engine CLI the agent drives, as a `HarnessKind` token. Defaults to
/// `claude_code`.
///
/// This one is Tidebreak's own, not part of the environment's contract: the
/// environment declares no engine selector for a custom workload, so the pod
/// specification that installs the agent also picks its engine. An
/// unrecognized token fails loudly rather than silently running the default,
/// because a typo here would otherwise drive the wrong engine for the whole
/// run.
pub const ENGINE_VARIABLE: &str = "TIDEBREAK_AGENT_ENGINE";

const DEFAULT_SUPERVISOR_ENDPOINT: &str = "127.0.0.1:15003";
/// Poll cadence, matching the endpoint's expected liveness rhythm.
pub const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Continuation policy after a successful turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunMode {
    /// Keep resuming until the goal ships or a bound stops the run.
    Goal,
    /// Park after each turn and wait for steering.
    Turn,
}

/// One declared repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repository {
    /// HTTPS URL as declared.
    pub url: String,
    /// Branch or commit, when one was declared.
    pub repository_ref: Option<String>,
}

/// Every input the agent reads, resolved and defaulted.
#[derive(Clone, Debug)]
pub struct Inputs {
    /// Task text for the first turn.
    pub task: String,
    /// Absolute URL of the control endpoint's poll route base.
    pub control_url: String,
    /// Directory the engine runs in.
    pub workspace: PathBuf,
    /// Continuation policy.
    pub mode: RunMode,
    /// Turn budget, when one was declared.
    pub max_turns: Option<u32>,
    /// Turn number this incarnation starts at.
    pub starting_turn: u32,
    /// Declared model route, when one was named.
    pub model: Option<String>,
    /// Requested reasoning effort, when one was named.
    pub reasoning_effort: Option<String>,
    /// Every declared repository, in declared order. Empty means research.
    pub repositories: Vec<Repository>,
    /// Whether pushes to the forge are denied.
    pub forge_push_denied: bool,
    /// Sandbox identifier, when the environment names one.
    pub sandbox_id: Option<String>,
    /// Incarnation counter, defaulting to 1.
    pub incarnation: u32,
    /// Engine CLI the agent drives.
    pub engine: HarnessKind,
}

/// A missing or unusable input, carrying the exit code and the variable name.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct InputError {
    /// Process exit code for a loud failure.
    pub code: i32,
    /// What is wrong, naming the variable.
    pub message: String,
}

impl InputError {
    fn missing(variable: &str) -> Self {
        Self {
            code: EXIT_MISSING_INPUT,
            message: format!("{variable} is required and was not set"),
        }
    }

    fn unusable(variable: &str, detail: &str) -> Self {
        Self {
            code: EXIT_MISSING_INPUT,
            message: format!("{variable} is unusable: {detail}"),
        }
    }
}

/// Raw environment values, separated from `std::env` so tests inject them.
#[derive(Clone, Debug, Default)]
pub struct RawInputs {
    pub task: Option<String>,
    pub supervisor_endpoint: Option<String>,
    pub workspace: Option<String>,
    pub mode: Option<String>,
    pub max_turns: Option<String>,
    pub starting_turn: Option<String>,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub repository_url: Option<String>,
    pub repository_ref: Option<String>,
    pub repositories: Option<String>,
    pub forge_push: Option<String>,
    pub sandbox_id: Option<String>,
    pub incarnation: Option<String>,
    pub engine: Option<String>,
}

impl RawInputs {
    /// Reads every variable from the process environment.
    #[must_use]
    pub fn from_env() -> Self {
        let var = |name: &str| std::env::var(name).ok();
        Self {
            task: var(TASK_VARIABLE),
            supervisor_endpoint: var(SUPERVISOR_ENDPOINT_VARIABLE),
            workspace: var(WORKSPACE_VARIABLE),
            mode: var(MODE_VARIABLE),
            max_turns: var(MAX_TURNS_VARIABLE),
            starting_turn: var(STARTING_TURN_VARIABLE),
            model: var(MODEL_VARIABLE),
            reasoning_effort: var(REASONING_EFFORT_VARIABLE),
            repository_url: var(REPOSITORY_URL_VARIABLE),
            repository_ref: var(REPOSITORY_REF_VARIABLE),
            repositories: var(REPOSITORIES_VARIABLE),
            forge_push: var(FORGE_PUSH_VARIABLE),
            sandbox_id: var(SANDBOX_ID_VARIABLE),
            incarnation: var(INCARNATION_VARIABLE),
            engine: var(ENGINE_VARIABLE),
        }
    }
}

/// Trims a value and treats empty the same as absent.
fn optional(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// Resolves every input, naming the first one that is absent or unusable.
pub fn resolve(raw: RawInputs) -> Result<Inputs, InputError> {
    let task = optional(raw.task).ok_or_else(|| InputError::missing(TASK_VARIABLE))?;

    let endpoint =
        optional(raw.supervisor_endpoint).unwrap_or_else(|| DEFAULT_SUPERVISOR_ENDPOINT.to_owned());
    let control_url = if endpoint.contains("://") {
        endpoint
    } else {
        format!("http://{endpoint}")
    };

    let workspace = optional(raw.workspace).map_or_else(
        || {
            std::env::current_dir().map_err(|error| {
                InputError::unusable(
                    WORKSPACE_VARIABLE,
                    &format!(
                        "no workspace was set and the working directory is unreadable: {error}"
                    ),
                )
            })
        },
        |value| Ok(PathBuf::from(value)),
    )?;

    let mode = match optional(raw.mode).as_deref() {
        None | Some("goal") => RunMode::Goal,
        Some("turn") => RunMode::Turn,
        Some(other) => {
            return Err(InputError::unusable(
                MODE_VARIABLE,
                &format!("expected \"goal\" or \"turn\", got {other:?}"),
            ))
        }
    };

    let max_turns = optional(raw.max_turns)
        .map(|value| {
            value.parse::<u32>().map_err(|error| {
                InputError::unusable(MAX_TURNS_VARIABLE, &format!("{value:?}: {error}"))
            })
        })
        .transpose()?;

    let starting_turn = optional(raw.starting_turn)
        .map(|value| {
            value.parse::<u32>().map_err(|error| {
                InputError::unusable(STARTING_TURN_VARIABLE, &format!("{value:?}: {error}"))
            })
        })
        .transpose()?
        .unwrap_or(1)
        .max(1);

    let repository_url = optional(raw.repository_url);
    // A ref without a repository is ignored — there is nothing to check out.
    let repository_ref = repository_url
        .as_ref()
        .and_then(|_| optional(raw.repository_ref));
    let repositories =
        parse_repositories(optional(raw.repositories).as_deref()).unwrap_or_else(|| {
            match repository_url {
                Some(url) => vec![Repository {
                    url,
                    repository_ref,
                }],
                None => Vec::new(),
            }
        });

    let forge_push_denied = optional(raw.forge_push).as_deref() == Some("denied");

    let incarnation = optional(raw.incarnation)
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(1);

    let engine = match optional(raw.engine) {
        None => HarnessKind::ClaudeCode,
        Some(token) => HarnessKind::from_str(&token).ok_or_else(|| {
            let known = HarnessKind::ALL
                .iter()
                .map(|kind| kind.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            InputError::unusable(
                ENGINE_VARIABLE,
                &format!("unknown engine {token:?}; expected one of {known}"),
            )
        })?,
    };

    Ok(Inputs {
        task,
        control_url,
        workspace,
        mode,
        max_turns,
        starting_turn,
        model: optional(raw.model),
        reasoning_effort: optional(raw.reasoning_effort),
        repositories,
        forge_push_denied,
        sandbox_id: optional(raw.sandbox_id),
        incarnation,
        engine,
    })
}

/// Parses the JSON repository list, keeping only usable entries.
///
/// Matches the environment's own parser: a malformed document or an empty
/// list yields `None` so the singular variables stay authoritative.
fn parse_repositories(raw: Option<&str>) -> Option<Vec<Repository>> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    let parsed: Vec<serde_json::Value> = serde_json::from_str(raw).ok()?;
    let repositories = parsed
        .into_iter()
        .filter_map(|entry| {
            let url = entry.get("url")?.as_str()?.trim().to_owned();
            if url.is_empty() {
                return None;
            }
            let repository_ref = entry
                .get("repository_ref")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned);
            Some(Repository {
                url,
                repository_ref,
            })
        })
        .collect::<Vec<_>>();
    (!repositories.is_empty()).then_some(repositories)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal() -> RawInputs {
        RawInputs {
            task: Some("do the thing".to_owned()),
            workspace: Some("/workspace".to_owned()),
            ..RawInputs::default()
        }
    }

    #[test]
    fn defaults_cover_every_optional_variable() {
        let inputs = resolve(minimal()).unwrap();
        assert_eq!(inputs.control_url, "http://127.0.0.1:15003");
        assert_eq!(inputs.mode, RunMode::Goal);
        assert_eq!(inputs.max_turns, None);
        assert_eq!(inputs.starting_turn, 1);
        assert_eq!(inputs.model, None);
        assert_eq!(inputs.reasoning_effort, None);
        assert!(inputs.repositories.is_empty());
        assert!(!inputs.forge_push_denied);
        assert_eq!(inputs.incarnation, 1);
    }

    /// The environment writes empty strings for undeclared fields; they must
    /// read the same as absent ones.
    #[test]
    fn empty_values_read_as_absent() {
        let raw = RawInputs {
            mode: Some(String::new()),
            model: Some("  ".to_owned()),
            repository_url: Some(String::new()),
            max_turns: Some(String::new()),
            ..minimal()
        };
        let inputs = resolve(raw).unwrap();
        assert_eq!(inputs.mode, RunMode::Goal);
        assert_eq!(inputs.model, None);
        assert!(inputs.repositories.is_empty());
        assert_eq!(inputs.max_turns, None);
    }

    #[test]
    fn missing_task_names_the_variable() {
        let error = resolve(RawInputs::default()).unwrap_err();
        assert_eq!(error.code, EXIT_MISSING_INPUT);
        assert!(error.message.contains(TASK_VARIABLE));
    }

    #[test]
    fn unknown_mode_fails_loudly_instead_of_becoming_goal() {
        let raw = RawInputs {
            mode: Some("gaol".to_owned()),
            ..minimal()
        };
        let error = resolve(raw).unwrap_err();
        assert!(error.message.contains(MODE_VARIABLE));
    }

    #[test]
    fn starting_turn_is_honored() {
        let raw = RawInputs {
            starting_turn: Some("4".to_owned()),
            ..minimal()
        };
        assert_eq!(resolve(raw).unwrap().starting_turn, 4);
    }

    #[test]
    fn endpoint_gains_a_scheme_only_when_missing() {
        let raw = RawInputs {
            supervisor_endpoint: Some("10.0.0.2:9000".to_owned()),
            ..minimal()
        };
        assert_eq!(resolve(raw).unwrap().control_url, "http://10.0.0.2:9000");
        let raw = RawInputs {
            supervisor_endpoint: Some("https://sidecar.local:9000".to_owned()),
            ..minimal()
        };
        assert_eq!(
            resolve(raw).unwrap().control_url,
            "https://sidecar.local:9000"
        );
    }

    #[test]
    fn repository_list_wins_over_the_singular_variables() {
        let raw = RawInputs {
            repository_url: Some("https://example.com/org/single.git".to_owned()),
            repository_ref: Some("main".to_owned()),
            repositories: Some(
                r#"[{"url": "https://example.com/org/a.git", "repository_ref": "dev"},
                    {"url": "https://example.com/org/b.git"}]"#
                    .to_owned(),
            ),
            ..minimal()
        };
        let inputs = resolve(raw).unwrap();
        assert_eq!(inputs.repositories.len(), 2);
        assert_eq!(inputs.repositories[0].url, "https://example.com/org/a.git");
        assert_eq!(
            inputs.repositories[0].repository_ref.as_deref(),
            Some("dev")
        );
        assert_eq!(inputs.repositories[1].repository_ref, None);
    }

    #[test]
    fn singular_repository_fills_in_when_the_list_is_malformed() {
        let raw = RawInputs {
            repository_url: Some("https://example.com/org/single.git".to_owned()),
            repository_ref: Some("main".to_owned()),
            repositories: Some("not json".to_owned()),
            ..minimal()
        };
        let inputs = resolve(raw).unwrap();
        assert_eq!(inputs.repositories.len(), 1);
        assert_eq!(
            inputs.repositories[0].repository_ref.as_deref(),
            Some("main")
        );
    }

    #[test]
    fn a_ref_without_a_repository_is_ignored() {
        let raw = RawInputs {
            repository_ref: Some("main".to_owned()),
            ..minimal()
        };
        assert!(resolve(raw).unwrap().repositories.is_empty());
    }

    #[test]
    fn the_engine_defaults_to_claude_code() {
        assert_eq!(resolve(minimal()).unwrap().engine, HarnessKind::ClaudeCode);
    }

    #[test]
    fn a_declared_engine_is_honored() {
        let raw = RawInputs {
            engine: Some("codex".to_owned()),
            ..minimal()
        };
        assert_eq!(resolve(raw).unwrap().engine, HarnessKind::Codex);
    }

    /// A typo must not silently drive the default engine for the whole run.
    #[test]
    fn an_unknown_engine_fails_naming_the_variable_and_the_tokens() {
        let raw = RawInputs {
            engine: Some("claude".to_owned()),
            ..minimal()
        };
        let error = resolve(raw).unwrap_err();
        assert_eq!(error.code, EXIT_MISSING_INPUT);
        assert!(error.message.contains(ENGINE_VARIABLE));
        assert!(error.message.contains("claude_code"));
    }
}
