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
use ts_rs::TS;

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
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
    /// A search of this conversation's own sources. The query is the whole
    /// action, and it is what the excerpts returned will be chosen to match.
    Search { query: String },
    /// A public web search. What leaves the device is the entire reason this
    /// call asks first, so the card has to be able to show all of it — the
    /// filters are told to the provider too, not just the query.
    WebSearch {
        query: String,
        /// Sites the search is confined to, empty when the model named none.
        domains: Vec<String>,
        /// Earliest publication date the search will accept, as the model
        /// wrote it. Kept verbatim rather than reformatted: the card's job is
        /// to show what the provider is actually told.
        start_published_at: Option<String>,
        /// Latest publication date the search will accept.
        end_published_at: Option<String>,
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
                let args = clamped_list(arguments.get("args"));
                let cwd = clamped_field(arguments.get("cwd")).unwrap_or_else(|| ".".into());
                Some(Self::Exec { command, args, cwd })
            }
            // Approving a search without seeing its query is not consent to
            // anything in particular, and for `web_search` that query is the
            // thing that leaves the machine. Trimmed, because the tool trims
            // before searching and a card should show what actually goes out.
            "search" => Some(Self::Search {
                query: search_query(arguments)?,
            }),
            "web_search" => Some(Self::WebSearch {
                query: search_query(arguments)?,
                domains: clamped_list(arguments.get("domains")),
                start_published_at: clamped_field(arguments.get("start_published_at")),
                end_published_at: clamped_field(arguments.get("end_published_at")),
            }),
            _ => None,
        }
    }

    /// Whether [`Self::build`] would reproduce `arguments` without losing
    /// anything.
    ///
    /// The preview is clamped so a card stays bounded: long fields are cut,
    /// control characters removed, and surplus or unreadable arguments dropped.
    /// That is right for showing a human what a call does and wrong for
    /// deciding whether a *later* call is the same one, because the clamp is
    /// many-to-one — a 600-character command and that command with anything
    /// appended project to the same 512 characters.
    ///
    /// A scope narrower than the whole tool may only be created from, or
    /// matched against, a call this returns `true` for. Anything else has to
    /// keep asking.
    #[must_use]
    pub fn describes_exactly(tool_name: &str, arguments: &Value) -> bool {
        match tool_name {
            "exec" => {
                arguments
                    .get("command")
                    .and_then(Value::as_str)
                    .is_some_and(survives_clamp)
                    // A dropped or elided argument silently changes the call's
                    // arity, and an absent `cwd` is faithfully the default the
                    // preview shows.
                    && list_survives_clamp(arguments.get("args"))
                    && field_survives_clamp(arguments.get("cwd"))
            }
            // A search's action is its query, so a grant may name that query.
            // `k` is not part of it: it bounds how many passages come back and
            // changes nothing about what leaves the machine.
            "search" => query_survives_clamp(arguments),
            // `max_results` bounds the response the same way, but the domain
            // and date filters are told to the provider alongside the query.
            // They are on the card for that reason, and a grant may only be
            // built from a call the card showed in full.
            "web_search" => {
                query_survives_clamp(arguments)
                    && list_survives_clamp(arguments.get("domains"))
                    && field_survives_clamp(arguments.get("start_published_at"))
                    && field_survives_clamp(arguments.get("end_published_at"))
            }
            // A tool with no variant projects nothing, so there is nothing a
            // narrower scope could name — and claiming otherwise would invite a
            // narrow grant with nothing to narrow to.
            _ => false,
        }
    }
}

/// What a call produced, in a form a human can read.
///
/// A command's output is the whole reason to run it. Withholding it leaves the
/// transcript asserting that something happened without ever showing what.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
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

/// The query a search will actually run, as the card should show it.
fn search_query(arguments: &Value) -> Option<String> {
    clamp(
        arguments.get("query")?.as_str()?.trim(),
        MAX_ACTION_FIELD_CHARS,
    )
}

/// Bound one optional single-line preview field, dropping control characters
/// that could forge card structure. An absent, empty, or all-control field is
/// not presentable.
fn clamped_field(value: Option<&Value>) -> Option<String> {
    clamp(value?.as_str()?, MAX_ACTION_FIELD_CHARS)
}

/// Bound a list of single-line preview fields: surplus entries elided,
/// unreadable ones dropped.
fn clamped_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .take(MAX_ACTION_ARGS)
                .filter_map(|value| {
                    value
                        .as_str()
                        .and_then(|value| clamp(value, MAX_ACTION_FIELD_CHARS))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Whether a query reaches the preview intact.
///
/// The trim is not a loss. Both search tools trim before searching, so padding
/// is not a difference between two calls; anything the clamp would actually
/// remove is.
fn query_survives_clamp(arguments: &Value) -> bool {
    arguments
        .get("query")
        .and_then(Value::as_str)
        .is_some_and(|query| survives_clamp(query.trim()))
}

/// Whether an optional single-line argument reaches the preview intact. An
/// absent one is faithfully the absence the preview shows.
fn field_survives_clamp(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(Value::String(text)) => survives_clamp(text),
        Some(_) => false,
    }
}

/// Whether a list argument reaches the preview intact.
///
/// An absent list is faithfully the empty one; a present one must be an array
/// whose every element survives, since a dropped or elided element silently
/// changes what the call does.
fn list_survives_clamp(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(Value::Array(values)) => {
            values.len() <= MAX_ACTION_ARGS
                && values
                    .iter()
                    .all(|value| value.as_str().is_some_and(survives_clamp))
        }
        Some(_) => false,
    }
}

/// Whether [`clamp`] would return this string unchanged.
///
/// Deliberately mirrors `clamp`'s three lossy steps rather than calling it and
/// comparing, so the two cannot drift into disagreeing about what "unchanged"
/// means: nothing removed, nothing truncated, and nothing that clamps away to
/// nothing.
fn survives_clamp(value: &str) -> bool {
    !value.is_empty()
        && !value.chars().any(char::is_control)
        && value.chars().count() <= MAX_ACTION_FIELD_CHARS
}

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
    use serde_json::json;

    /// A `web_search` action with no filters, which is the common shape.
    fn plain_web_search(query: &str) -> ToolActionPreview {
        ToolActionPreview::WebSearch {
            query: query.into(),
            domains: Vec::new(),
            start_published_at: None,
            end_published_at: None,
        }
    }

    #[test]
    fn a_search_shows_the_query_it_is_asking_permission_for() {
        // Approving `web_search` used to show nothing about the query, which is
        // the one thing that actually leaves the machine.
        assert_eq!(
            ToolActionPreview::build("web_search", &json!({ "query": "quarterly filings" })),
            Some(plain_web_search("quarterly filings"))
        );
        assert_eq!(
            ToolActionPreview::build("search", &json!({ "query": "revenue" })),
            Some(ToolActionPreview::Search {
                query: "revenue".into()
            })
        );
        // Trimmed, because the tool trims before searching.
        assert_eq!(
            ToolActionPreview::build("web_search", &json!({ "query": "  spaced  " })),
            Some(plain_web_search("spaced"))
        );
        // Extra arguments are not part of the action under review.
        assert_eq!(
            ToolActionPreview::build("search", &json!({ "query": "revenue", "k": 8 })),
            Some(ToolActionPreview::Search {
                query: "revenue".into()
            })
        );
        // A query the card cannot show is no preview at all, rather than a card
        // that describes an empty search.
        for arguments in [
            json!({}),
            json!({ "query": "" }),
            json!({ "query": "   " }),
            json!({ "query": 3 }),
        ] {
            assert_eq!(ToolActionPreview::build("search", &arguments), None);
        }
    }

    #[test]
    fn a_web_search_shows_the_filters_that_go_out_with_the_query() {
        // The consent copy promises the query "and its explicit filters", and
        // the filters reach the provider too — a card that showed only the
        // query was describing part of the action it asked about.
        assert_eq!(
            ToolActionPreview::build(
                "web_search",
                &json!({
                    "query": "quarterly filings",
                    "max_results": 10,
                    "domains": ["sec.gov", "ft.com"],
                    "start_published_at": "2024-01-01T00:00:00Z",
                }),
            ),
            Some(ToolActionPreview::WebSearch {
                query: "quarterly filings".into(),
                domains: vec!["sec.gov".into(), "ft.com".into()],
                start_published_at: Some("2024-01-01T00:00:00Z".into()),
                end_published_at: None,
            })
        );
    }

    #[test]
    fn an_action_is_exactly_describable_only_when_the_card_showed_all_of_it() {
        assert!(ToolActionPreview::describes_exactly(
            "exec",
            &json!({ "command": "cargo", "args": ["test"] })
        ));
        // A search names its query exactly, which is what lets an approval
        // offer a grant narrower than every search in the chat.
        for tool in ["search", "web_search"] {
            assert!(ToolActionPreview::describes_exactly(
                tool,
                &json!({ "query": "anything" })
            ));
        }
        // Result counts bound the response, not the disclosure, so they are not
        // part of the action a grant names.
        assert!(ToolActionPreview::describes_exactly(
            "search",
            &json!({ "query": "anything", "k": 8 })
        ));
        assert!(ToolActionPreview::describes_exactly(
            "web_search",
            &json!({ "query": "anything", "max_results": 10 })
        ));
        // Filters are disclosure, and the card carries them, so a filtered
        // search is exact too.
        assert!(ToolActionPreview::describes_exactly(
            "web_search",
            &json!({ "query": "anything", "domains": ["sec.gov"] })
        ));
        // A filter the card could not show is a filter nobody consented to.
        for arguments in [
            json!({ "query": "anything", "domains": ["se\u{0}c.gov"] }),
            json!({ "query": "anything", "domains": [7] }),
            json!({ "query": "anything", "domains": "sec.gov" }),
            json!({ "query": "anything", "end_published_at": 20_240_101 }),
            json!({ "query": "any\u{0}thing" }),
        ] {
            assert!(!ToolActionPreview::describes_exactly(
                "web_search",
                &arguments
            ));
        }
        // A tool with no variant has nothing to be exact about.
        assert!(!ToolActionPreview::describes_exactly(
            "mcp__server__tool",
            &json!({ "query": "anything" })
        ));
    }

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
            (
                "write_file",
                serde_json::json!({ "path": "/Users/private" }),
            ),
            ("mcp__server__tool", serde_json::json!({ "any": "thing" })),
        ] {
            assert_eq!(ToolActionPreview::build(tool, &arguments), None);
            assert_eq!(ToolResultPreview::build(tool, Some(&arguments)), None);
        }

        // The searches project an *action* so their query can be reviewed, and
        // deliberately no *result*: what a search returned is the answer the
        // model works from, not something the transcript restates.
        for tool in ["search", "web_search"] {
            let arguments = serde_json::json!({ "query": "private" });
            assert!(ToolActionPreview::build(tool, &arguments).is_some());
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
