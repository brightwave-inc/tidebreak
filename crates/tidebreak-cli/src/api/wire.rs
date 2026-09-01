//! Client-side decode of the server's JSON.
//!
//! The chat event socket's frames are the server's own types, imported from
//! `tidebreak_server::wire`: one Rust definition serializes on the server and
//! deserializes here, so a renamed field is a compile error in this crate the
//! way it is a type error in the desktop renderer's generated `wire.ts`. The
//! contract those types carry — closed vocabularies, unknown keys rejected, an
//! unknown event type failing its frame — is documented on that module.
//!
//! The REST records below are still hand-written mirrors. Their server types
//! only serialize today, so they cannot be imported the same way; they read
//! only the fields a CLI surface uses and let serde drop the rest, and the
//! opaque strings they carry are bounded with the same
//! [`tidebreak_server::wire::limits`] the renderer applies. Moving them onto
//! the server's types is tracked in brightwave-inc/tidebreak#3005.

use serde::Deserialize;
use tidebreak_core::CallId;
pub use tidebreak_core::{
    Chat, PendingPlanApproval, PendingUserQuestions, RendererToolName, ToolActionPreview,
    ToolApprovalKind,
};
pub use tidebreak_server::wire::limits;
pub use tidebreak_server::wire::{
    AgentActivityHistoryItem, ApprovalGrantRung, RendererAgentEvent, RendererChatFrame,
    RendererToolStatus,
};

/// A timestamp string within [`limits::MAX_WIRE_TIMESTAMP_CHARS`] code points.
///
/// The output routes carry timestamps as preformatted strings rather than
/// `DateTime`s, so the bound is the only check they get; the renderer applies
/// the same number.
fn bounded_timestamp<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    if value.chars().count() > limits::MAX_WIRE_TIMESTAMP_CHARS {
        return Err(serde::de::Error::custom(format!(
            "timestamp exceeds {} characters",
            limits::MAX_WIRE_TIMESTAMP_CHARS
        )));
    }
    Ok(value)
}

/// One background agent run, from `GET /chats/{id}/agent-runs`.
#[derive(Debug, Clone, Deserialize)]
pub struct AgentRunSnapshot {
    pub id: tidebreak_core::AgentRunId,
    #[serde(default)]
    pub parent_id: Option<tidebreak_core::AgentRunId>,
    #[serde(default)]
    pub tier: Option<String>,
    /// Where the agent run loop itself executes (`in_process` / `container`).
    /// Not the code-exec backend — see [`Self::code_execution_provider`].
    #[serde(default)]
    pub execution_location: Option<String>,
    /// Host code-execution backend for `exec` (`local` / `e2b` / `docker` /
    /// `daytona` / `off`). Independent of [`Self::execution_location`].
    #[serde(default)]
    pub code_execution_provider: Option<String>,
    pub status: String,
    #[serde(default)]
    pub task: Option<String>,
    #[serde(default)]
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub last_error_code: Option<String>,
    /// Files the run named in its terminal `done` submission.
    #[serde(default)]
    pub submitted_outputs: Vec<SubmittedOutput>,
    #[serde(default)]
    pub terminal_text: Option<String>,
    #[serde(default)]
    pub spawn_call_id: Option<CallId>,
    #[serde(default)]
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// One file a background run submitted, as the list route returns it.
#[derive(Debug, Clone, Deserialize)]
pub struct SubmittedOutput {
    pub output_id: tidebreak_core::OutputId,
    pub filename: String,
}

/// One selectable model, from `GET /models`.
#[derive(Debug, Clone, Deserialize)]
pub struct ModelInfo {
    pub key: String,
    pub id: String,
    pub display_name: String,
    /// The provider that serves the model; decoded for forward compatibility,
    /// not yet grouped on in the picker.
    #[allow(dead_code)]
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub available: bool,
    #[serde(default)]
    pub context_window: u32,
    #[serde(default)]
    pub reasoning_efforts: Vec<String>,
}

/// The model catalog response.
#[derive(Debug, Clone, Deserialize)]
pub struct ModelCatalog {
    #[serde(default)]
    pub models: Vec<ModelInfo>,
    /// Every named model role, its selection, and what it resolves to now.
    #[serde(default)]
    pub roles: Vec<ModelRoleInfo>,
}

/// One named model role, from `GET /models`.
#[derive(Debug, Clone, Deserialize)]
pub struct ModelRoleInfo {
    pub role: String,
    /// The catalog key pinned to this role, or `None` for automatic.
    #[serde(default)]
    pub selection: Option<String>,
    /// What the role resolves to right now, selection or not.
    #[serde(default)]
    pub resolved_key: Option<String>,
}

/// One provider row from `GET /providers`.
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderInfo {
    /// Wire form of the provider kind (`anthropic`, `openai_compatible`, …),
    /// which is also the path segment the write routes take.
    pub kind: String,
    #[serde(default)]
    pub enabled: bool,
    /// Whether a credential is stored — never the credential itself.
    #[serde(default)]
    pub has_credential: bool,
    /// How the provider is authenticated (`api_key`, `chatgpt`, …), when it is.
    #[serde(default)]
    pub auth_mode: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
}

/// The `GET /providers` envelope.
#[derive(Debug, Clone, Deserialize)]
pub struct ProvidersList {
    #[serde(default)]
    pub providers: Vec<ProviderInfo>,
}

/// One mounted MCP server, from `GET /mcp/servers`. The response flattens each
/// server's definition into its live projection, so both are read here.
#[derive(Debug, Clone, Deserialize)]
pub struct McpServerInfo {
    pub name: String,
    #[serde(default)]
    pub enabled: bool,
    /// `initializing` / `healthy` / `degraded` / `reconnecting` / `disabled`.
    #[serde(default)]
    pub health: String,
    #[serde(default)]
    pub tool_count: usize,
    /// The plugin this server came from, when it is plugin-sourced. Those are
    /// derived by the server and cannot be edited over the config route.
    #[serde(default)]
    pub plugin: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub gateway_endpoint: Option<String>,
    #[serde(default)]
    pub diagnostic: Option<String>,
}

/// The `GET /mcp/servers` envelope.
#[derive(Debug, Clone, Deserialize)]
pub struct McpServersInfo {
    #[serde(default)]
    pub servers: Vec<McpServerInfo>,
}

/// One row of a conversation's outputs catalog.
///
/// The output routes answer in the shape the desktop renderer already
/// validates, which is why these fields are camelCase where the rest of the
/// API is not — moving the surface off Tauri was a transport change, not a
/// payload change.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputSummary {
    pub output_id: tidebreak_core::OutputId,
    pub filename: String,
    pub media_type: String,
    pub size_bytes: u64,
    pub revision_count: u32,
    #[serde(deserialize_with = "bounded_timestamp")]
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputsCatalog {
    pub deliverables: Vec<OutputSummary>,
    /// Whether the conversation has more outputs than one answer carries.
    pub truncated: bool,
}

/// One output's bounded text preview.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputPreview {
    pub filename: String,
    pub media_type: String,
    pub revision_id: tidebreak_core::OutputRevisionId,
    pub content: String,
    pub truncated: bool,
}

/// One row of an output's version history.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputRevisionRow {
    pub revision_id: tidebreak_core::OutputRevisionId,
    pub ordinal: u32,
    pub size_bytes: u64,
    #[serde(deserialize_with = "bounded_timestamp")]
    pub created_at: String,
    pub produced_by: String,
    pub is_current: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputRevisions {
    pub revisions: Vec<OutputRevisionRow>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Path of the server's chat-frame fixtures, relative to this crate.
    const CHAT_FRAMES: &str = "../tidebreak-server/fixtures/chat-frames.json";

    fn chat_frame_fixtures() -> Vec<(String, serde_json::Value)> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(CHAT_FRAMES);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let entries: Vec<serde_json::Value> =
            serde_json::from_str(&text).expect("the fixture file is a JSON array");
        entries
            .into_iter()
            .map(|entry| {
                let name = entry["name"]
                    .as_str()
                    .expect("every fixture is named")
                    .to_owned();
                (name, entry["frame"].clone())
            })
            .collect()
    }

    /// Every frame the server can send decodes here, byte for byte. The
    /// fixtures are serialized from the server's own types by a test in
    /// `tidebreak-server`, and the renderer's tests read the same file, so
    /// the three decoders cannot drift apart without one of them failing.
    #[test]
    fn every_server_chat_frame_decodes() {
        let fixtures = chat_frame_fixtures();
        assert!(fixtures.len() > 20, "the fixture list looks truncated");
        for (name, frame) in fixtures {
            let decoded: RendererChatFrame = serde_json::from_value(frame.clone())
                .unwrap_or_else(|error| panic!("fixture {name} does not decode: {error}"));
            let again = serde_json::to_value(&decoded).expect("a decoded frame serializes");
            assert_eq!(again, frame, "fixture {name} changed across the round trip");
        }
    }

    /// The CLI reads the cursor from event frames and ignores metadata frames;
    /// both kinds are in the fixtures, so pin which is which.
    #[test]
    fn fixtures_carry_both_frame_kinds() {
        let mut events = 0;
        let mut metadata = 0;
        for (_, frame) in chat_frame_fixtures() {
            match serde_json::from_value::<RendererChatFrame>(frame).expect("decodes") {
                RendererChatFrame::Event(frame) => {
                    assert!(frame.seq > 0);
                    events += 1;
                }
                RendererChatFrame::Metadata(_) => metadata += 1,
            }
        }
        assert!(events > 0 && metadata > 0);
    }

    /// The hand-written output mirrors apply the renderer's timestamp bound.
    #[test]
    fn output_timestamps_are_bounded_like_the_renderer() {
        let row = |updated_at: &str| {
            serde_json::json!({
                "outputId": "00000000-0000-0000-0000-000000000001",
                "filename": "report.md",
                "mediaType": "text/markdown",
                "sizeBytes": 12,
                "revisionCount": 1,
                "updatedAt": updated_at,
            })
        };
        assert!(serde_json::from_value::<OutputSummary>(row("2026-09-01T12:00:00Z")).is_ok());
        let too_long = "x".repeat(limits::MAX_WIRE_TIMESTAMP_CHARS + 1);
        assert!(serde_json::from_value::<OutputSummary>(row(&too_long)).is_err());
    }
}
