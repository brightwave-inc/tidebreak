//! The tool contract: how the agent invokes a capability.
//!
//! Every tool is a typed args/result pair with a JSON Schema — no
//! stringly-typed tools. Tools come from three sources (built-in, skill-backed,
//! MCP-mounted) but all implement this one trait so the registry treats them
//! uniformly.

use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;
use crate::id::ChatId;

/// The approval policy class a tool declares for itself.
///
/// Policy maps class → auto-approve / ask / deny. In v1: `ReadOnly` and
/// `Workspace` auto-approve; `Sensitive` always parks on the approval gate.
/// (Workspace-outside prompting and standing grants are deferred.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalClass {
    /// Never mutates anything (e.g. `read_file`, `list_dir`, `search`).
    ReadOnly,
    /// Mutates the chat workspace (e.g. `write_file`).
    Workspace,
    /// Escapes the workspace or reaches the network / external services
    /// (connector writes, networked `exec`, writes outside the workspace).
    Sensitive,
}

/// A tool's public contract: name, description, and the JSON Schema its
/// arguments must satisfy. This is what gets advertised to the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    /// Unique tool name (MCP-mounted tools are namespaced `mcp__{server}__{tool}`).
    pub name: String,
    /// Human- and model-readable description of what the tool does.
    pub description: String,
    /// JSON Schema (draft 2020-12) describing the argument object.
    pub input_schema: Value,
}

/// The result of executing a tool.
///
/// `content` is the model-readable result folded back into the conversation;
/// `data` is an optional structured payload for clients that can render it
/// (e.g. a tool-call card). A failing tool returns `is_error = true` rather than
/// an `Err` so the model sees the failure and can adapt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolOutput {
    /// Result text fed back to the model.
    pub content: String,
    /// Optional structured payload for richer client rendering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    /// Whether the tool reported a failure.
    #[serde(default)]
    pub is_error: bool,
}

impl ToolOutput {
    /// A successful text result.
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            data: None,
            is_error: false,
        }
    }

    /// A failure the model should see and react to.
    pub fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            data: None,
            is_error: true,
        }
    }

    /// Attach a structured payload to this output.
    #[must_use]
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }
}

/// Execution context handed to a tool for one invocation.
///
/// Deliberately minimal in this slice — it grows (cancellation, store handles)
/// as the agent loop lands.
#[derive(Debug, Clone)]
pub struct ToolCtx {
    /// The chat this call belongs to.
    pub chat_id: ChatId,
    /// Absolute path to the chat's workspace directory. Workspace-class
    /// tools stay within it without prompting.
    pub workspace_dir: PathBuf,
}

/// A capability the agent can invoke. Implementors are held as trait objects in
/// the registry, so this trait must stay object-safe (hence `#[async_trait]`).
#[async_trait]
pub trait Tool: Send + Sync {
    /// The tool's advertised contract.
    fn spec(&self) -> ToolSpec;

    /// The approval class governing this tool's calls.
    fn approval_class(&self) -> ApprovalClass;

    /// Execute the tool with JSON `args` matching [`ToolSpec::input_schema`].
    async fn execute(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_output_constructors_set_error_flag() {
        assert!(!ToolOutput::text("ok").is_error);
        assert!(ToolOutput::error("boom").is_error);
    }

    #[test]
    fn tool_output_omits_absent_data_when_serialized() {
        let json = serde_json::to_string(&ToolOutput::text("ok")).unwrap();
        assert!(
            !json.contains("data"),
            "absent data should be skipped: {json}"
        );

        let with = ToolOutput::text("ok").with_data(serde_json::json!({"k": 1}));
        assert_eq!(with.data, Some(serde_json::json!({"k": 1})));
    }

    #[test]
    fn approval_class_serializes_snake_case() {
        let json = serde_json::to_string(&ApprovalClass::ReadOnly).unwrap();
        assert_eq!(json, "\"read_only\"");
    }
}
