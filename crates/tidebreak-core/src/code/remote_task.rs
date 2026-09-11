//! Versioned workspace metadata carried through the runtime's task string.

use serde::{Deserialize, Serialize};

const PREFIX: &str = "tidebreak-workspace-task\n";
const VERSION: u8 = 2;

/// A Tidebreak workspace's first task, its remote checkout branch, and its
/// repository-optional scratch posture. The supervisor consumes the metadata
/// before handing the task to the engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteWorkspaceTask {
    version: u8,
    pub task: String,
    pub branch: String,
    /// True when the workspace is repository-less: the engine runs on a
    /// session-private scratch directory and nothing is cloned.
    #[serde(default)]
    pub scratch: bool,
}

impl RemoteWorkspaceTask {
    /// Wrap a task for a Tidebreak supervisor without changing its text.
    pub fn encode(task: &str, branch: &str) -> Result<String, serde_json::Error> {
        let envelope = Self {
            version: VERSION,
            task: task.to_owned(),
            branch: branch.to_owned(),
            scratch: false,
        };
        Ok(format!("{PREFIX}{}", serde_json::to_string(&envelope)?))
    }

    /// Wrap a task for a repository-less supervised session. The workspace
    /// branch exists only so every envelope stays branch-bearing; the agent
    /// must run without cloning anything.
    pub fn encode_scratch(task: &str, branch: &str) -> Result<String, serde_json::Error> {
        let envelope = Self {
            version: VERSION,
            task: task.to_owned(),
            branch: branch.to_owned(),
            scratch: true,
        };
        Ok(format!("{PREFIX}{}", serde_json::to_string(&envelope)?))
    }

    /// Decode Tidebreak metadata, leaving ordinary custom tasks untouched.
    pub fn parse(value: &str) -> Result<Option<Self>, String> {
        let Some(json) = value.strip_prefix(PREFIX) else {
            return Ok(None);
        };
        let envelope: Self = serde_json::from_str(json)
            .map_err(|error| format!("the workspace task envelope is invalid: {error}"))?;
        if !matches!(envelope.version, 1 | VERSION) || (envelope.version == 1 && envelope.scratch) {
            return Err("the workspace task envelope version is unsupported".into());
        }
        if envelope.task.trim().is_empty() {
            return Err("the workspace task is empty".into());
        }
        if envelope.branch.is_empty()
            || envelope.branch.len() > 1024
            || envelope.branch != envelope.branch.trim()
            || envelope.branch.starts_with('-')
            || envelope.branch.contains("@{")
        {
            return Err("the workspace branch is invalid".into());
        }
        Ok(Some(envelope))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_envelope_preserves_task_text_and_branch() {
        let task = "  Keep \"this\" text.\nThen test it.\n";
        let encoded = RemoteWorkspaceTask::encode(task, "thet/slack-pr").unwrap();
        let parsed = RemoteWorkspaceTask::parse(&encoded).unwrap().unwrap();
        assert_eq!(parsed.task, task);
        assert_eq!(parsed.branch, "thet/slack-pr");
        assert_eq!(RemoteWorkspaceTask::parse(task).unwrap(), None);
    }

    #[test]
    fn version_one_repository_tasks_remain_compatible() {
        let parsed = RemoteWorkspaceTask::parse(&format!(
            "{PREFIX}{}",
            r#"{"version":1,"task":"work","branch":"thet/task"}"#
        ))
        .unwrap()
        .unwrap();
        assert_eq!(parsed.task, "work");
        assert!(!parsed.scratch);
    }

    #[test]
    fn a_scratch_envelope_runs_without_a_repository() {
        let task = "Synthesize a report";
        let encoded = RemoteWorkspaceTask::encode_scratch(task, "scratch/one").unwrap();
        let parsed = RemoteWorkspaceTask::parse(&encoded).unwrap().unwrap();
        assert_eq!(parsed.task, task);
        assert_eq!(parsed.branch, "scratch/one");
        assert!(parsed.scratch);
    }

    #[test]
    fn invalid_envelopes_fail_instead_of_becoming_engine_instructions() {
        for body in [
            r#"{"version":3,"task":"work","branch":"task"}"#,
            r#"{"version":1,"task":"work","branch":"task","scratch":true}"#,
            r#"{"version":1,"task":"work","branch":"task","extra":true}"#,
            r#"{"version":1,"task":" ","branch":"task"}"#,
            r#"{"version":1,"task":"work","branch":"-reset"}"#,
            r#"{"version":1,"task":"work","branch":"@{-1}"}"#,
            "not json",
        ] {
            assert!(RemoteWorkspaceTask::parse(&format!("{PREFIX}{body}")).is_err());
        }
    }
}
