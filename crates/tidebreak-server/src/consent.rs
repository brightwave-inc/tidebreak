//! The unified consent read model.
//!
//! "What may the agent do without asking me" is answered by two stores with no
//! shared type: standing tool grants in the journal database, and host-broker
//! capability grants over connected folders. A [`ConsentStatementSnapshot`] is
//! one row of the union — subject, verb, resource, consent provenance, and a
//! revocation handle — so the settings surface can render every statement the
//! user has made in one list. This is deliberately a read model: enforcement
//! stays where it is (the approval gate for tools, `authorize()` in the
//! broker), and these rows are projections of the records those consult.
//!
//! The server can only produce the tool-grant half; capability grants live in
//! the desktop's host broker and reach the renderer over the Tauri boundary in
//! this same shape, which is why the capability vocabulary here is `pub` and
//! constructible from outside the crate.

use chrono::{DateTime, Utc};
use serde::Serialize;

use tidebreak_core::{CallId, GrantLevel, GrantScope, RendererToolName, ToolApprovalKind};

/// One statement of consent the agent currently holds, whatever store it
/// lives in.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
pub struct ConsentStatementSnapshot {
    /// What a revocation of this statement names, and where to send it.
    pub handle: ConsentHandle,
    /// How far the statement reaches — one chat, or every chat in a project.
    pub level: GrantLevel,
    /// The name of whatever the level points at, for provenance. `None` when
    /// that chat or project is untitled.
    pub level_title: Option<String>,
    /// The class of action the user allowed.
    pub verb: ConsentVerb,
    /// What the verb is allowed to touch.
    pub resource: ConsentResource,
    /// The trusted interaction through which consent was captured.
    pub method: ConsentMethodSnapshot,
    pub granted_at: DateTime<Utc>,
}

/// The durable identity a revocation names.
///
/// The two stores revoke differently: a tool grant is withdrawn through
/// `DELETE /grants/{call_id}`, while a capability grant is not individually
/// revocable yet — today the Folders surface disconnects the whole root, and
/// per-statement withdrawal arrives when the boundary is derived from these
/// statements.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConsentHandle {
    /// The approval decision that created a standing tool grant.
    ToolGrant { call_id: CallId },
    /// The host broker's stable identity for a capability grant.
    CapabilityGrant { grant_id: String },
}

/// The class of action a consent statement allows.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConsentVerb {
    /// A gated tool call the user said "always allow" to.
    Tool {
        action: RendererToolName,
        approval: ToolApprovalKind,
    },
    /// A host action class guarded by the broker.
    Capability { capability: HostCapability },
}

/// Renderer-safe mirror of the host broker's capability vocabulary.
///
/// The server does not link the broker crate, so the boundary vocabulary is
/// restated here for the wire; the desktop maps the broker's own enum into
/// this one when it assembles capability statements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum HostCapability {
    /// Discover safe summaries of connected folders.
    ListRoots,
    /// List directories and read file bytes within a connected folder.
    ReadFiles,
    /// Create or explicitly approved replacement of files in a connected
    /// folder.
    WriteFiles,
    /// Expose a connected folder to model-authored commands.
    ExecuteCommands,
}

/// What a consent statement's verb is allowed to touch.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConsentResource {
    /// The action scope of a standing tool grant, verbatim.
    ActionScope { scope: GrantScope },
    /// A subject-wide host action that touches no particular folder.
    HostSubject,
    /// An entire connected folder, named by the same safe identity the
    /// folders surface shows — never an absolute path.
    HostRoot {
        root_id: String,
        display_name: Option<String>,
    },
    /// One subtree within a connected folder.
    HostPathSubtree {
        root_id: String,
        display_name: Option<String>,
        relative: String,
    },
}

/// The trusted interaction that captured a consent statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ConsentMethodSnapshot {
    /// "Always allow" on an approval card.
    ApprovalCard,
    /// A folder chosen through the trusted native picker.
    FolderPicker,
    /// An approved folder saved for automatic use in future chats.
    TrustedFolder,
    /// An explicit permission dialog.
    PermissionDialog,
    /// A local operator deliberately provisioned a headless installation.
    OperatorConfig,
    /// A state migration named reach an existing consent already conveyed;
    /// the timestamp is the original consent's, not a fresh approval.
    CarriedForward,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidebreak_core::{ChatId, ProjectId};
    use uuid::Uuid;

    /// Both halves of the union serialize to the tagged JSON the renderer's
    /// generated types describe. The desktop builds capability rows in another
    /// process, so this shape is a cross-process contract, not an internal.
    #[test]
    fn both_consent_statement_flavors_serialize_to_the_shared_wire_shape() {
        let chat_id = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
        let call_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let granted_at: DateTime<Utc> = "2026-07-29T12:00:00Z".parse().unwrap();

        let tool = ConsentStatementSnapshot {
            handle: ConsentHandle::ToolGrant {
                call_id: CallId(call_id),
            },
            level: GrantLevel::Chat {
                chat_id: ChatId::from(chat_id),
            },
            level_title: Some("Quarterly filings".into()),
            verb: ConsentVerb::Tool {
                action: tidebreak_core::RendererToolName::Exec,
                approval: ToolApprovalKind::ExecMayRunNetworkedCommand,
            },
            resource: ConsentResource::ActionScope {
                scope: GrantScope::CommandPrefix {
                    tokens: vec!["cargo".into(), "test".into()],
                },
            },
            method: ConsentMethodSnapshot::ApprovalCard,
            granted_at,
        };
        assert_eq!(
            serde_json::to_value(&tool).unwrap(),
            serde_json::json!({
                "handle": {"kind": "tool_grant", "call_id": call_id},
                "level": {"level": "chat", "chat_id": chat_id},
                "level_title": "Quarterly filings",
                "verb": {
                    "kind": "tool",
                    "action": "exec",
                    "approval": "exec_may_run_networked_command",
                },
                "resource": {
                    "kind": "action_scope",
                    "scope": {"scope": "command_prefix", "tokens": ["cargo", "test"]},
                },
                "method": "approval_card",
                "granted_at": "2026-07-29T12:00:00Z",
            })
        );

        let project_id = Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap();
        let capability = ConsentStatementSnapshot {
            handle: ConsentHandle::CapabilityGrant {
                grant_id: "44444444-4444-4444-4444-444444444444".into(),
            },
            level: GrantLevel::Project {
                project_id: ProjectId::from(project_id),
            },
            level_title: None,
            verb: ConsentVerb::Capability {
                capability: HostCapability::WriteFiles,
            },
            resource: ConsentResource::HostRoot {
                root_id: "55555555-5555-5555-5555-555555555555".into(),
                display_name: Some("Documents".into()),
            },
            method: ConsentMethodSnapshot::FolderPicker,
            granted_at,
        };
        assert_eq!(
            serde_json::to_value(&capability).unwrap(),
            serde_json::json!({
                "handle": {
                    "kind": "capability_grant",
                    "grant_id": "44444444-4444-4444-4444-444444444444",
                },
                "level": {"level": "project", "project_id": project_id},
                "level_title": null,
                "verb": {"kind": "capability", "capability": "write_files"},
                "resource": {
                    "kind": "host_root",
                    "root_id": "55555555-5555-5555-5555-555555555555",
                    "display_name": "Documents",
                },
                "method": "folder_picker",
                "granted_at": "2026-07-29T12:00:00Z",
            })
        );
    }
}
