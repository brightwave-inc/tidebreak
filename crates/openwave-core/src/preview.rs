//! Closed renderer projections of what a tool will do, and what it did.
//!
//! The renderer boundary carries no tool arguments and no tool output. These
//! types are the deliberate exceptions: a tool may opt in to showing a human
//! the action under review and the result it produced, field by field.
//!
//! They are not passthroughs. Each variant enumerates exactly what a person
//! needs in order to consent to an action or to understand its outcome, values
//! are clamped, and a tool without a variant projects nothing.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Longest command, argument, or directory string an action preview carries.
pub const MAX_ACTION_FIELD_CHARS: usize = 512;

/// Most arguments an action preview carries before it elides the tail.
pub const MAX_ACTION_ARGS: usize = 32;

/// Longest captured stream a result preview carries. The execution provider
/// already bounds what it captures; this bounds what crosses the boundary.
pub const MAX_RESULT_STREAM_CHARS: usize = 40_000;

/// The action a call will take, in a form a human can inspect.
///
/// Approval cards need this because consent to an action you cannot see is not
/// consent. Result cards reuse it so the same action is described the same way
/// before and after it runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "tool", rename_all = "snake_case")]
pub enum ToolActionPreview {
    /// A command execution, as the argument vector it will actually run. There
    /// is no shell string to show because no shell parses it.
    Exec {
        /// Executable name or path the model chose.
        command: String,
        /// Arguments passed directly to the executable.
        args: Vec<String>,
        /// Working directory relative to the chat's private scratch, never a
        /// host path.
        cwd: String,
    },
}

impl ToolActionPreview {
    /// Project the action for a call, or `None` when the tool has none.
    ///
    /// `arguments` are the canonical model-authored arguments. Only the fields
    /// named by a variant are read; malformed arguments yield `None` rather
    /// than degrading the card into a raw JSON dump.
    #[must_use]
    pub fn build(tool_name: &str, arguments: &Value) -> Option<Self> {
        match tool_name {
            // Kept beside `ToolApprovalKind::for_tool_name`, which already owns
            // the closed mapping from tool name to consent semantics.
            "exec" => {
                let command = clamp(arguments.get("command")?.as_str()?, MAX_ACTION_FIELD_CHARS)?;
                let args = arguments
                    .get("args")
                    .and_then(Value::as_array)
                    .map(|args| {
                        args.iter()
                            .take(MAX_ACTION_ARGS)
                            .filter_map(|arg| {
                                arg.as_str()
                                    .and_then(|arg| clamp(arg, MAX_ACTION_FIELD_CHARS))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let cwd = arguments
                    .get("cwd")
                    .and_then(Value::as_str)
                    .and_then(|cwd| clamp(cwd, MAX_ACTION_FIELD_CHARS))
                    .unwrap_or_else(|| ".".into());
                Some(Self::Exec { command, args, cwd })
            }
            _ => None,
        }
    }
}

/// What a call produced, in a form a human can read.
///
/// A command's output is the whole reason to run it. Withholding it leaves the
/// transcript asserting that something happened without ever showing what.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "tool", rename_all = "snake_case")]
pub enum ToolResultPreview {
    Exec {
        /// Process exit status, or `None` when it was killed by a signal.
        exit_code: Option<i32>,
        /// Whether the provider stopped the command at its time limit.
        timed_out: bool,
        /// Whether the provider dropped output past its capture limit.
        output_truncated: bool,
        stdout: String,
        stderr: String,
    },
}

impl ToolResultPreview {
    /// Project the result of a call from the tool's own structured output.
    ///
    /// `data` is [`crate::ToolOutput::data`], which is otherwise private. A
    /// tool opts in by putting the enumerated fields there; everything else it
    /// carries stays behind the boundary.
    #[must_use]
    pub fn build(tool_name: &str, data: Option<&Value>) -> Option<Self> {
        match tool_name {
            "exec" => {
                let data = data?;
                Some(Self::Exec {
                    exit_code: data
                        .get("exit_code")
                        .and_then(Value::as_i64)
                        .and_then(|code| i32::try_from(code).ok()),
                    timed_out: data
                        .get("timed_out")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    output_truncated: data
                        .get("output_truncated")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    stdout: stream(data.get("stdout")),
                    stderr: stream(data.get("stderr")),
                })
            }
            _ => None,
        }
    }

    /// Whether this result has any captured output to show.
    #[must_use]
    pub fn has_output(&self) -> bool {
        match self {
            Self::Exec { stdout, stderr, .. } => !stdout.is_empty() || !stderr.is_empty(),
        }
    }
}

/// Bound a captured stream, keeping its newlines: output without line breaks
/// is not output anyone can read.
fn stream(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_str)
        .map(|text| {
            text.chars()
                .filter(|character| {
                    !character.is_control() || *character == '\n' || *character == '\t'
                })
                .take(MAX_RESULT_STREAM_CHARS)
                .collect()
        })
        .unwrap_or_default()
}

/// Bound one single-line preview field, dropping control characters that could
/// forge card structure. An empty or all-control field is not presentable.
fn clamp(value: &str, max_chars: usize) -> Option<String> {
    let cleaned: String = value
        .chars()
        .filter(|character| !character.is_control())
        .take(max_chars)
        .collect();
    (!cleaned.is_empty()).then_some(cleaned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_action_carries_the_argument_vector_and_defaults_its_directory() {
        assert_eq!(
            ToolActionPreview::build(
                "exec",
                &serde_json::json!({ "command": "python3", "args": ["-c", "print(1)"] }),
            ),
            Some(ToolActionPreview::Exec {
                command: "python3".into(),
                args: vec!["-c".into(), "print(1)".into()],
                cwd: ".".into(),
            })
        );
    }

    #[test]
    fn only_tools_with_a_variant_project_anything() {
        for (tool, arguments) in [
            ("search", serde_json::json!({ "query": "private" })),
            ("web_search", serde_json::json!({ "query": "private" })),
            (
                "write_file",
                serde_json::json!({ "path": "/Users/private" }),
            ),
            ("mcp__server__tool", serde_json::json!({ "any": "thing" })),
        ] {
            assert_eq!(ToolActionPreview::build(tool, &arguments), None);
            assert_eq!(ToolResultPreview::build(tool, Some(&arguments)), None);
        }
    }

    #[test]
    fn malformed_exec_arguments_yield_no_preview_rather_than_a_raw_dump() {
        for arguments in [
            serde_json::json!({}),
            serde_json::json!({ "command": "" }),
            serde_json::json!({ "command": 7 }),
            serde_json::json!("not an object"),
        ] {
            assert_eq!(ToolActionPreview::build("exec", &arguments), None);
        }
    }

    #[test]
    fn exec_action_bounds_the_card_a_model_can_paint() {
        let long = "a".repeat(MAX_ACTION_FIELD_CHARS * 2);
        let many: Vec<_> = (0..MAX_ACTION_ARGS * 2).map(|i| i.to_string()).collect();
        let Some(ToolActionPreview::Exec { command, args, cwd }) = ToolActionPreview::build(
            "exec",
            &serde_json::json!({
                "command": long,
                "args": many,
                // Control characters could otherwise forge card structure.
                "cwd": "scratch\n\u{1b}[31mapproved",
            }),
        ) else {
            panic!("exec projects an action");
        };
        assert_eq!(command.chars().count(), MAX_ACTION_FIELD_CHARS);
        assert_eq!(args.len(), MAX_ACTION_ARGS);
        assert_eq!(cwd, "scratch[31mapproved");
    }

    #[test]
    fn exec_result_carries_the_streams_and_the_exit_status() {
        let result = ToolResultPreview::build(
            "exec",
            Some(&serde_json::json!({
                "exit_code": 1,
                "timed_out": false,
                "output_truncated": true,
                "stdout": "line one\nline two\n",
                "stderr": "boom\n",
            })),
        );
        assert_eq!(
            result,
            Some(ToolResultPreview::Exec {
                exit_code: Some(1),
                timed_out: false,
                output_truncated: true,
                stdout: "line one\nline two\n".into(),
                stderr: "boom\n".into(),
            })
        );
        assert!(result.unwrap().has_output());
    }

    #[test]
    fn a_signal_kill_has_no_exit_code_and_a_silent_run_has_no_output() {
        let Some(result) = ToolResultPreview::build(
            "exec",
            Some(&serde_json::json!({ "exit_code": null, "timed_out": true })),
        ) else {
            panic!("exec projects a result");
        };
        assert_eq!(
            result,
            ToolResultPreview::Exec {
                exit_code: None,
                timed_out: true,
                output_truncated: false,
                stdout: String::new(),
                stderr: String::new(),
            }
        );
        assert!(!result.has_output());
    }

    #[test]
    fn captured_streams_keep_their_line_breaks_but_stay_bounded() {
        let Some(ToolResultPreview::Exec { stdout, stderr, .. }) = ToolResultPreview::build(
            "exec",
            Some(&serde_json::json!({
                "stdout": "a".repeat(MAX_RESULT_STREAM_CHARS * 2),
                "stderr": "kept\nlines\n\u{1b}[31mbut not escapes",
            })),
        ) else {
            panic!("exec projects a result");
        };
        assert_eq!(stdout.chars().count(), MAX_RESULT_STREAM_CHARS);
        assert_eq!(stderr, "kept\nlines\n[31mbut not escapes");
    }
}
