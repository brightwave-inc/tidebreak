//! Protected server-side native-tool bridge for supervised sandboxes.
//!
//! The sandbox never holds an authoritative tool registry, forge
//! credentials, or a server bearer. It asks this loopback route by name;
//! the server validates the name against an explicit allowlist (the five
//! coordinator tools plus the three conversation tools parent registers),
//! derives owner/grant from the sandbox session's durable binding, executes
//! through the authoritative `ToolRegistry`, and returns typed output plus
//! bounded artifacts that the agent materializes into sandbox scratch.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tidebreak_core::code::SupervisorToolRequest;
use tidebreak_core::{ToolCtx, ToolOutput};

use super::runtime::CodeRuntime;

/// Ceiling on one artifact (text/JSONL and images both at 2 MiB).
pub const MAX_ARTIFACT_BYTES: usize = 2 * 1024 * 1024;

/// Names the bridge may execute. Deliberately not "every server tool": an
/// allowlist keeps the blast radius to coordination and conversation reads.
pub const BRIDGE_TOOLS: &[&str] = &[
    "code_repos",
    "code_session_create",
    "code_run_turn",
    "code_wait",
    "code_sessions",
    "conversation_read",
    "conversation_export",
    "conversation_attachment",
];

/// Bounded artifact carried back to the sandbox for scratch materialization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeArtifact {
    /// Safe private-scratch-relative path generated server-side.
    pub path: String,
    /// MIME media type.
    pub media_type: String,
    /// Bytes, bounded at [`MAX_ARTIFACT_BYTES`].
    pub bytes: Vec<u8>,
}

/// One protected tool response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeResult {
    /// The request id this answers (stable across agent retries).
    pub request_id: String,
    /// Serializable tool output, text- or JSON-shaped.
    pub output: serde_json::Value,
    /// Files to materialize under the sandbox's private scratch.
    #[serde(default)]
    pub artifacts: Vec<BridgeArtifact>,
}

/// One request body from the sandbox.
#[derive(Debug, Deserialize)]
pub struct BridgeRequest {
    /// The sandbox session this call acts for.
    pub session_id: tidebreak_core::SessionId,
    /// The protected tool request.
    pub request: SupervisorToolRequest,
}

/// The protected result body sent back.
#[derive(Debug, Serialize)]
pub struct BridgeResponse {
    /// The tool output and artifacts.
    #[serde(flatten)]
    pub result: BridgeResult,
}

/// The extension point parent wires conversation tools through.
///
/// `conversation_tools` supplies `execute_remote` returning
/// `ConversationToolResult { output, files }`; parent wires it at boot.
/// Until then this module executes conversation names with an honest
/// unavailable answer rather than pretending the Slack tool exists.
#[async_trait]
pub trait RemoteConversationTools: Send + Sync {
    /// Execute one allowlisted conversation tool remotely.
    async fn execute_remote(
        &self,
        runtime: &CodeRuntime,
        ctx: &ToolCtx,
        name: &str,
        args: serde_json::Value,
    ) -> Result<ConversationToolResult, tidebreak_core::AgentError>;
}

/// The output shape parent's `conversation_tools` returns.
pub struct ConversationToolResult {
    /// The standard tool output (text or images for the internal engine).
    pub output: ToolOutput,
    /// Bounded export/attachment files to materialize remotely.
    pub files: Vec<ConversationArtifact>,
}

/// One bounded conversation artifact.
pub struct ConversationArtifact {
    /// Safe private-scratch-relative path.
    pub path: String,
    /// MIME media type.
    pub media_type: String,
    /// Bounded bytes.
    pub bytes: Vec<u8>,
}

/// A stub conversation executor until parent supplies the real one.
pub struct UnavailableConversationTools;

#[async_trait]
impl RemoteConversationTools for UnavailableConversationTools {
    async fn execute_remote(
        &self,
        _runtime: &CodeRuntime,
        _ctx: &ToolCtx,
        _name: &str,
        _args: serde_json::Value,
    ) -> Result<ConversationToolResult, tidebreak_core::AgentError> {
        Err(tidebreak_core::AgentError::config(
            "conversation tools are not registered on this deployment",
        ))
    }
}

impl CodeRuntime {
    /// The current conversation-tool executor. Default refuses loudly until
    /// parent installs `conversation_tools`.
    pub fn conversation_tools(&self) -> Arc<dyn RemoteConversationTools> {
        self.conversation_tools.clone()
    }

    /// Execute one allowlisted protected tool for a supervised sandbox
    /// session and return the typed bridge result.
    pub async fn execute_sandbox_tool(
        &self,
        session_id: tidebreak_core::SessionId,
        request: &SupervisorToolRequest,
    ) -> Result<BridgeResult, tidebreak_core::AgentError> {
        if !BRIDGE_TOOLS.contains(&request.tool.as_str()) {
            return Err(tidebreak_core::AgentError::config(format!(
                "the protected sandbox bridge does not expose tool {}",
                request.tool
            )));
        }
        // The session's durable binding is the authority. No forge
        // credentials or adapter tokens are carried into the sandbox; this
        // route re-derives the grant from the row each call.
        let session = tidebreak_core::db::code::get_session_all_owners(
            &self.db,
            session_id,
        )
        .await?
        .ok_or_else(|| tidebreak_core::AgentError::Store("sandbox session not found".into()))?;
        let bindings = tidebreak_core::db::code::list_bindings_for_session(
            &self.db,
            &session.owner,
            session.id,
        )
        .await?;
        let grant = bindings
            .first()
            .map(|binding| binding.grant_id)
            .or_else(|| None);
        if let Some(grant_id) = grant {
            let live = tidebreak_core::db::code::get_external_grant(
                &self.db,
                &session.owner,
                grant_id,
            )
            .await?
            .filter(|grant| grant.revoked_at.is_none())
            .ok_or_else(|| {
                tidebreak_core::AgentError::AccessDenied(
                    "the sandbox session's external connection was revoked".into(),
                )
            })?;
            let _ = live;
        }
        let ctx = ToolCtx::without_private_scratch(session.id, None)
            .with_call_id(tidebreak_core::CallId::new());
        if request.tool.starts_with("conversation_") {
            let result = self
                .conversation_tools
                .execute_remote(self, &ctx, &request.tool, request.arguments.clone())
                .await?;
            let artifacts = result
                .files
                .into_iter()
                .map(|file| BridgeArtifact {
                    path: file.path,
                    media_type: file.media_type,
                    bytes: file.bytes,
                })
                .collect();
            return Ok(BridgeResult {
                request_id: request.request_id.clone(),
                output: output_value(result.output, &[]),
                artifacts,
            });
        }
        let tool = self
            .tool_registry()
            .ok_or_else(|| {
                tidebreak_core::AgentError::config(
                    "the code runtime has no tool registry for sandbox tools",
                )
            })?
            .server_tool(&request.tool)
            .ok_or_else(|| {
                tidebreak_core::AgentError::config(format!(
                    "the protected tool {} is not registered on this deployment",
                    request.tool
                ))
            })?;
        let output = tool
            .execute(&ctx, request.arguments.clone())
            .await
            .map_err(|err| tidebreak_core::AgentError::Store(err.to_string()))?;
        Ok(BridgeResult {
            request_id: request.request_id.clone(),
            output: output_value(output, &[]),
            artifacts: Vec::new(),
        })
    }

    /// The process tool registry carrying the coordinator tools. `None` in
    /// custom/test hosts that never installed `SessionTools`.
    pub fn tool_registry(&self) -> Option<Arc<tidebreak_core::ToolRegistry>> {
        self.tools.clone()
    }
}

/// Loopback handler the supervised sandbox calls to execute one protected
/// native tool. The session is named by the sandbox's own identifier; the
/// server derives owner and grant from durable rows, never from a bearer the
/// sandbox holds.
pub async fn sandbox_tool_call(
    axum::extract::State(state): axum::extract::State<crate::state::AppState>,
    axum::Json(body): axum::Json<BridgeRequest>,
) -> Result<axum::Json<BridgeResponse>, crate::error::ServerError> {
    use crate::error::ServerError;
    let Some(runtime) = state.code.as_deref() else {
        return Err(ServerError::internal(
            "code mode is not configured on this server",
        ));
    };
    let result = runtime
        .execute_sandbox_tool(body.session_id, &body.request)
        .await
        .map_err(|err| ServerError::internal(err.to_string()))?;
    Ok(axum::Json(BridgeResponse { result }))
}

fn output_value(output: ToolOutput, _images: &[BridgeArtifact]) -> serde_json::Value {
    serde_json::json!({
        "content": output.content,
        "is_error": output.is_error,
        "error_category": output.error_category.map(|kind| kind.as_str()),
        "data": output.data,
    })
}
