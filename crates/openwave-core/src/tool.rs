//! The tool contract: how the agent invokes a capability.
//!
//! Every tool is a typed args/result pair with a JSON Schema — no
//! stringly-typed tools. Tools come from three sources (built-in, skill-backed,
//! MCP-mounted) but all implement this one trait so the registry treats them
//! uniformly.

use std::path::PathBuf;
#[cfg(feature = "tools")]
use std::sync::Arc;

#[cfg(feature = "tools")]
use cap_std::ambient_authority;
#[cfg(feature = "tools")]
use cap_std::fs::Dir;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;
use crate::id::{CallId, ChatId, ProjectId};

/// A pinned runtime-only directory capability for legacy private-scratch tools.
///
/// It carries no host path and grants access only to the already-open directory
/// handle supplied by the embedding runtime.
#[derive(Clone)]
pub struct ToolScratch {
    #[cfg(feature = "tools")]
    workspace: Arc<Dir>,
}

impl std::fmt::Debug for ToolScratch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolScratch")
            .field("available", &cfg!(feature = "tools"))
            .finish_non_exhaustive()
    }
}

impl ToolScratch {
    /// Wrap an already-open exact directory capability.
    #[cfg(feature = "tools")]
    #[must_use]
    pub fn from_dir(workspace: Dir) -> Self {
        Self {
            workspace: Arc::new(workspace),
        }
    }
}

/// The approval policy class a tool declares for itself.
///
/// Policy maps class → auto-approve / ask / deny. In v1: `ReadOnly` and
/// `Workspace` auto-approve; `Sensitive` parks on the approval gate unless a
/// matching standing grant covers the call.
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

impl ApprovalClass {
    /// Stable database and wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::Workspace => "workspace",
            Self::Sensitive => "sensitive",
        }
    }
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
/// Why a tool call failed.
///
/// A boolean says something went wrong; a category says whether anything *is*
/// wrong. A call the reader cancelled and a call the reader declined are not
/// failures of the tool, of the model, or of the product, and recording them
/// the same way as a crash makes every later question about reliability
/// unanswerable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ToolErrorCategory {
    /// The reader stopped the turn before or during the call.
    UserCancelled,
    /// The reader declined the call at the approval gate.
    UserDeclined,
    /// The model named a tool this turn does not advertise.
    NotFound,
    /// The tool ran and reported a failure of its own.
    ToolFailed,
}

impl ToolErrorCategory {
    /// Stable durable and wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UserCancelled => "user_cancelled",
            Self::UserDeclined => "user_declined",
            Self::NotFound => "not_found",
            Self::ToolFailed => "tool_failed",
        }
    }

    /// Whether this counts against the product rather than describing a choice
    /// the reader made or a request the model got wrong.
    #[must_use]
    pub const fn is_product_failure(self) -> bool {
        match self {
            Self::UserCancelled | Self::UserDeclined | Self::NotFound => false,
            Self::ToolFailed => true,
        }
    }
}

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
    /// Why it failed, when it did. `None` on success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_category: Option<ToolErrorCategory>,
    /// Server-private durable sidecar committed with the canonical tool result.
    #[serde(skip)]
    pub private_evidence: Vec<crate::RetrievalEvidenceInput>,
}

impl ToolOutput {
    /// A successful text result.
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            data: None,
            is_error: false,
            error_category: None,
            private_evidence: Vec::new(),
        }
    }

    /// A failure the model should see and react to.
    ///
    /// Categorized as the tool's own failure. Callers that know better — the
    /// loop, which is the only thing that can tell a cancellation from a
    /// crash — use [`Self::failed`].
    pub fn error(content: impl Into<String>) -> Self {
        Self::failed(ToolErrorCategory::ToolFailed, content)
    }

    /// A failure whose cause the caller can name.
    pub fn failed(category: ToolErrorCategory, content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            data: None,
            is_error: true,
            error_category: Some(category),
            private_evidence: Vec::new(),
        }
    }

    /// Attach a structured payload to this output.
    #[must_use]
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }

    /// Attach bounded evidence that never serializes into events or renderer DTOs.
    #[must_use]
    pub fn with_private_evidence(mut self, evidence: Vec<crate::RetrievalEvidenceInput>) -> Self {
        self.private_evidence = evidence;
        self
    }
}

/// Execution context handed to a tool for one invocation.
///
/// Deliberately minimal in this slice — it grows (cancellation, store handles)
/// as the agent loop lands.
#[derive(Clone)]
pub struct ToolCtx {
    /// The chat this call belongs to.
    pub chat_id: ChatId,
    /// Project corpus inherited from the chat, or `None` for a loose chat.
    pub project_id: Option<ProjectId>,
    /// Stable identity of the canonical tool call, when execution came from an
    /// agent turn. Legacy direct/MCP contexts leave this absent.
    pub call_id: Option<CallId>,
    #[cfg(feature = "tools")]
    workspace: WorkspaceAccess,
}

#[cfg(feature = "tools")]
#[derive(Clone)]
enum WorkspaceAccess {
    Open(Arc<Dir>),
    Unavailable(Arc<str>),
}

impl std::fmt::Debug for ToolCtx {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolCtx")
            .field("chat_id", &self.chat_id)
            .field("project_id", &self.project_id)
            .field("call_id", &self.call_id)
            .field("private_scratch_available", &self.scratch_available())
            .finish_non_exhaustive()
    }
}

impl ToolCtx {
    /// Build a legacy CLI/MCP context by opening an explicit workspace path.
    ///
    /// Product turns must use a pinned [`ToolScratch`] supplied by their runtime.
    pub fn new_legacy_workspace(
        chat_id: ChatId,
        project_id: Option<ProjectId>,
        workspace_dir: PathBuf,
    ) -> Self {
        match Self::try_new_legacy_workspace(chat_id, project_id, workspace_dir) {
            Ok(ctx) => ctx,
            Err(_error) => Self {
                chat_id,
                project_id,
                call_id: None,
                #[cfg(feature = "tools")]
                workspace: WorkspaceAccess::Unavailable(_error.to_string().into()),
            },
        }
    }

    /// Build a legacy CLI/MCP context, failing if its path cannot be pinned.
    pub fn try_new_legacy_workspace(
        chat_id: ChatId,
        project_id: Option<ProjectId>,
        workspace_dir: PathBuf,
    ) -> std::io::Result<Self> {
        #[cfg(feature = "tools")]
        let workspace = Dir::open_ambient_dir(&workspace_dir, ambient_authority())?;
        Ok(Self {
            chat_id,
            project_id,
            call_id: None,
            #[cfg(feature = "tools")]
            workspace: WorkspaceAccess::Open(Arc::new(workspace)),
        })
    }

    /// Build a product execution context from an exact pinned scratch handle.
    #[must_use]
    pub fn with_private_scratch(
        chat_id: ChatId,
        project_id: Option<ProjectId>,
        scratch: ToolScratch,
    ) -> Self {
        Self {
            chat_id,
            project_id,
            call_id: None,
            #[cfg(feature = "tools")]
            workspace: WorkspaceAccess::Open(scratch.workspace),
        }
    }

    /// Build a context with no direct filesystem scratch available.
    ///
    /// Non-filesystem tools remain usable; a legacy filesystem tool fails
    /// closed instead of resolving an absent path against the process CWD.
    #[must_use]
    pub fn without_private_scratch(chat_id: ChatId, project_id: Option<ProjectId>) -> Self {
        Self {
            chat_id,
            project_id,
            call_id: None,
            #[cfg(feature = "tools")]
            workspace: WorkspaceAccess::Unavailable("private scratch is unavailable".into()),
        }
    }

    /// Attach the canonical tool-call identity to this invocation context.
    #[must_use]
    pub fn with_call_id(mut self, call_id: CallId) -> Self {
        self.call_id = Some(call_id);
        self
    }

    fn scratch_available(&self) -> bool {
        #[cfg(feature = "tools")]
        {
            matches!(&self.workspace, WorkspaceAccess::Open(_))
        }
        #[cfg(not(feature = "tools"))]
        {
            false
        }
    }

    #[cfg(feature = "tools")]
    pub(crate) fn workspace(&self) -> std::result::Result<Arc<Dir>, String> {
        match &self.workspace {
            WorkspaceAccess::Open(workspace) => Ok(Arc::clone(workspace)),
            WorkspaceAccess::Unavailable(error) => {
                Err(format!("private scratch unavailable: {error}"))
            }
        }
    }
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
    #[test]
    fn a_reader_choice_is_not_recorded_as_a_product_failure() {
        // The distinction this exists for: a cancelled or declined call is a
        // choice, not a defect, and counting it as one makes any later question
        // about reliability unanswerable.
        for category in [
            ToolErrorCategory::UserCancelled,
            ToolErrorCategory::UserDeclined,
            ToolErrorCategory::NotFound,
        ] {
            assert!(!category.is_product_failure(), "{}", category.as_str());
        }
        assert!(ToolErrorCategory::ToolFailed.is_product_failure());

        // Every category has a distinct durable spelling.
        let spellings = [
            ToolErrorCategory::UserCancelled,
            ToolErrorCategory::UserDeclined,
            ToolErrorCategory::NotFound,
            ToolErrorCategory::ToolFailed,
        ]
        .map(ToolErrorCategory::as_str);
        assert_eq!(
            spellings.len(),
            spellings
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
        );
    }

    #[test]
    fn an_uncategorized_failure_reads_as_the_tool_failing() {
        // `error` is what an ordinary tool calls, and a tool reporting a failure
        // is exactly the tool failing. Only the loop can distinguish more.
        let plain = ToolOutput::error("boom");
        assert!(plain.is_error);
        assert_eq!(plain.error_category, Some(ToolErrorCategory::ToolFailed));

        let cancelled = ToolOutput::failed(ToolErrorCategory::UserCancelled, "stopped");
        assert_eq!(
            cancelled.error_category,
            Some(ToolErrorCategory::UserCancelled)
        );

        // Success carries no category at all rather than a benign-looking one.
        assert_eq!(ToolOutput::text("fine").error_category, None);
        assert!(!ToolOutput::text("fine").is_error);
    }

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
    fn private_evidence_never_serializes() {
        let document_id = crate::DocumentId::new();
        let output =
            ToolOutput::text("ok").with_private_evidence(vec![crate::RetrievalEvidenceInput {
                rank: 1,
                source_token: uuid::Uuid::new_v4(),
                document_id,
                generation: crate::DocumentGeneration {
                    content_revision: 1,
                    revision_token: uuid::Uuid::new_v4(),
                },
                chunk_id: crate::ChunkId::derive(document_id, 0, 6),
                span: crate::ByteSpan::new(0, 6),
                snippet: "secret".into(),
                heading_path: Vec::new(),
                source_regions: Vec::new(),
                source: crate::RetrievalEvidenceSource::Inline,
            }]);
        let json = serde_json::to_string(&output).unwrap();
        assert!(!json.contains("secret"));
        assert!(!json.contains("private_evidence"));
        assert_eq!(output.private_evidence.len(), 1);
    }

    #[test]
    fn approval_class_serializes_snake_case() {
        let json = serde_json::to_string(&ApprovalClass::ReadOnly).unwrap();
        assert_eq!(json, "\"read_only\"");
    }
}
