//! The foreground `memory` tool contract.
//!
//! One tool, three verbs: `propose` drafts a durable record for the user to
//! review, `search` runs the backend's lexical search, and `read` loads one
//! record verbatim. The spec and argument parser live here, beside the domain
//! model, so every surface that mounts the verb validates the same call shape
//! (mirroring how [`crate::task_plan`] shares one schema across surfaces). The
//! executor lives with the host, which owns the backend handle and the owner
//! resolution.
//!
//! A `propose` is deliberately not a write with authority: the executor stores
//! it as a `proposed` record (decision 0067), so nothing a model sends through
//! this tool changes what injects into later sessions until the user activates
//! it.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::memory::{MemoryKind, MAX_MEMORY_BODY_BYTES, MAX_MEMORY_TITLE_CHARS};
use crate::ToolSpec;

/// Stable tool name for the foreground memory verb.
pub const MEMORY_TOOL: &str = "memory";

/// Most search hits one `search` call returns to the model.
pub const MEMORY_TOOL_SEARCH_LIMIT: usize = 8;

/// What one `memory` call does.
// Undocumented variants on purpose: a `schemars` unit enum whose variants
// carry doc comments generates `oneOf` + `const`, which the strict schema
// subset providers enforce has no form for. The meaning lives on the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryToolVerb {
    Propose,
    Search,
    Read,
}

/// Canonical model arguments for [`MEMORY_TOOL`].
///
/// The schema is one flat object because the strict subset providers enforce
/// cannot express per-verb requirements; [`parse_memory_tool_arguments`]
/// checks them and answers with correction text the model can act on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryToolArgs {
    /// What to do: `propose` drafts one durable memory for the user to
    /// review, `search` finds stored memories by text, `read` loads one
    /// record by id.
    pub verb: MemoryToolVerb,
    /// For `propose`: the knowledge category — `fact`, `preference`,
    /// `lesson`, or `reference`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<MemoryKind>,
    /// For `propose`: one plain line stating when this memory matters, so a
    /// later session can decide from the title alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = MAX_MEMORY_TITLE_CHARS))]
    pub title: Option<String>,
    /// For `propose`: the memory itself, as short markdown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = MAX_MEMORY_BODY_BYTES))]
    pub body: Option<String>,
    /// For `search`: the text to look for across stored titles and bodies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1))]
    pub query: Option<String>,
    /// For `read`: the id of the record to load, exactly as a search hit or
    /// an earlier result named it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_id: Option<String>,
}

/// Parse and check one call's arguments, or say exactly what to fix.
///
/// The registry already rejects anything the advertised schema forbids; this
/// covers what one flat schema cannot express — which fields each verb needs.
///
/// # Errors
///
/// Returns model-facing correction text when the arguments are unusable.
pub fn parse_memory_tool_arguments(
    arguments: &Value,
) -> std::result::Result<MemoryToolArgs, String> {
    let parsed: MemoryToolArgs = serde_json::from_value(arguments.clone())
        .map_err(|error| format!("memory arguments are not valid: {error}"))?;
    match parsed.verb {
        MemoryToolVerb::Propose => {
            if parsed.kind.is_none() {
                return Err(
                    "propose needs `kind`: one of fact, preference, lesson, or reference"
                        .to_owned(),
                );
            }
            let title_missing = parsed
                .title
                .as_deref()
                .is_none_or(|title| title.trim().is_empty());
            if title_missing {
                return Err(
                    "propose needs `title`: one plain line saying when this memory matters"
                        .to_owned(),
                );
            }
            if parsed
                .body
                .as_deref()
                .is_none_or(|body| body.trim().is_empty())
            {
                return Err("propose needs `body`: the memory itself, as short markdown".to_owned());
            }
        }
        MemoryToolVerb::Search => {
            if parsed
                .query
                .as_deref()
                .is_none_or(|query| query.trim().is_empty())
            {
                return Err("search needs `query`: the text to look for".to_owned());
            }
        }
        MemoryToolVerb::Read => {
            if parsed.record_id.is_none() {
                return Err(
                    "read needs `record_id`: the id a search hit or earlier result named"
                        .to_owned(),
                );
            }
        }
    }
    Ok(parsed)
}

/// The advertised foreground spec.
#[must_use]
pub fn memory_tool_spec() -> ToolSpec {
    ToolSpec::for_args::<MemoryToolArgs>(
        MEMORY_TOOL,
        "Work with the user's durable memory. verb=search finds stored memories by text; \
         verb=read loads one record by id; verb=propose drafts one new memory — a stable fact, \
         stated preference, reusable lesson, or durable reference worth keeping beyond this \
         conversation — for the user to review. A proposal is a draft, not a saved memory: it \
         carries no authority until the user activates it, so never describe it as remembered. \
         Do not propose secrets, transient task state, or anything the user asked to keep out \
         of memory.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_verb_names_the_field_it_is_missing() {
        let propose = serde_json::json!({"verb": "propose", "kind": "fact"});
        let error = parse_memory_tool_arguments(&propose).expect_err("title is required");
        assert!(error.contains("title"), "{error}");

        let search = serde_json::json!({"verb": "search"});
        let error = parse_memory_tool_arguments(&search).expect_err("query is required");
        assert!(error.contains("query"), "{error}");

        let read = serde_json::json!({"verb": "read"});
        let error = parse_memory_tool_arguments(&read).expect_err("record_id is required");
        assert!(error.contains("record_id"), "{error}");

        let unknown = serde_json::json!({"verb": "search", "query": "x", "extra": true});
        assert!(parse_memory_tool_arguments(&unknown).is_err());

        let valid = serde_json::json!({
            "verb": "propose",
            "kind": "preference",
            "title": "When formatting reports",
            "body": "Use tables, not prose."
        });
        let parsed = parse_memory_tool_arguments(&valid).expect("the sample is valid");
        assert_eq!(parsed.kind, Some(MemoryKind::Preference));
    }

    #[test]
    fn the_advertised_schema_stays_in_the_strict_subset() {
        let schema = memory_tool_spec().input_schema;
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["required"], serde_json::json!(["verb"]));
        assert_eq!(
            schema["properties"]["verb"]["enum"],
            serde_json::json!(["propose", "search", "read"])
        );
        // Optional in the flat schema, so the enum admits the explicit null
        // an omitting model may send.
        assert_eq!(
            schema["properties"]["kind"]["enum"],
            serde_json::json!(["fact", "preference", "lesson", "reference", null])
        );
        assert_eq!(
            schema["properties"]["title"]["maxLength"],
            MAX_MEMORY_TITLE_CHARS
        );
        assert!(crate::tool::strict_json_schema(
            &schema,
            crate::tool::OptionalProperties::AcceptNull
        )
        .is_some());
    }
}
