//! The renderer's tool vocabulary.
//!
//! Tool names are model-controlled. Only names with a fixed renderer
//! presentation cross the boundary; everything else folds to
//! [`RendererToolName::Other`] rather than becoming a display path for a
//! provider-supplied string.
//!
//! This lives in `openwave-core` because three places need the same closed set:
//! the live event projection in the server, the history lookup that rebuilds a
//! terminal tool card from the journal, and [`ChatToolActivitySnapshot`]'s own
//! field type. Two of those are here, and until this module existed each kept
//! its own copy of the list. The copies drifted: `exec` was missing from the
//! history lookup, so a command relabelled itself as a generic tool on reload.
//!
//! [`ChatToolActivitySnapshot`]: crate::storage::ChatToolActivitySnapshot

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// A tool name the renderer is allowed to present.
///
/// The desktop's union, its runtime guard, its copy table, and its icon table
/// are all generated from this enum, so a variant added here cannot leave one of
/// them behind — see `docs/wire-types.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum RendererToolName {
    Search,
    ListSources,
    ReadSource,
    ReadToolResult,
    WebSearch,
    WebExtract,
    ReadDelegatedFile,
    ReadFile,
    ListDir,
    WriteFile,
    RequestFolderAccess,
    ConnectFolder,
    ListConnectedFolders,
    ListFolder,
    ReadConnectedFile,
    ImportConnectedFile,
    WriteOutputToConnectedFolder,
    SpawnSandboxAgent,
    WaitForAgents,
    AskUserQuestions,
    ExitPlanMode,
    UpdateTaskPlan,
    Exec,
    CreateApp,
    /// The fold for anything unrecognized, including a tool that has since been
    /// removed and any name a provider invented.
    Other,
}

impl From<&str> for RendererToolName {
    /// Fold a registered tool name onto the vocabulary.
    ///
    /// Written as literals rather than against the server's tool-name constants,
    /// which this crate cannot see. `source_tools_use_fixed_renderer_names` in
    /// the server holds those constants to these spellings.
    fn from(name: &str) -> Self {
        match name {
            "search" => Self::Search,
            "list_sources" => Self::ListSources,
            "read_source" => Self::ReadSource,
            "read_tool_result" => Self::ReadToolResult,
            "web_search" => Self::WebSearch,
            crate::WEB_EXTRACT_TOOL => Self::WebExtract,
            crate::SANDBOX_READ_DELEGATED_FILE_TOOL => Self::ReadDelegatedFile,
            "read_file" => Self::ReadFile,
            "list_dir" => Self::ListDir,
            "write_file" => Self::WriteFile,
            "request_folder_access" => Self::RequestFolderAccess,
            "connect_folder" => Self::ConnectFolder,
            "list_connected_folders" => Self::ListConnectedFolders,
            "list_folder" => Self::ListFolder,
            "read_connected_file" => Self::ReadConnectedFile,
            "import_connected_file" => Self::ImportConnectedFile,
            crate::WRITE_OUTPUT_TO_CONNECTED_FOLDER_TOOL => Self::WriteOutputToConnectedFolder,
            "spawn_sandbox_agent" => Self::SpawnSandboxAgent,
            "wait_for_agents" => Self::WaitForAgents,
            crate::ASK_USER_QUESTIONS_TOOL => Self::AskUserQuestions,
            crate::EXIT_PLAN_MODE_TOOL => Self::ExitPlanMode,
            crate::UPDATE_TASK_PLAN_TOOL => Self::UpdateTaskPlan,
            "exec" => Self::Exec,
            crate::local_app::CREATE_APP_TOOL => Self::CreateApp,
            _ => Self::Other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every registered tool name the renderer can present must survive the
    /// fold, both live and when a terminal card is rebuilt from the journal.
    ///
    /// `exec` used to be absent from the history lookup, which was a second copy
    /// of this mapping, so a command read as "Ran a command" while streaming and
    /// "Used a tool" after a reload — with its own command card still visible
    /// underneath. One enum now serves both, and this covers the behaviour that
    /// regressed.
    ///
    /// The input list is the registered *tool* names, which are not derivable
    /// from this enum: the fold exists precisely because the two vocabularies
    /// are different sets that happen to agree on spelling.
    #[test]
    fn every_renderer_visible_tool_keeps_its_name() {
        for name in [
            "search",
            "list_sources",
            "read_source",
            "read_tool_result",
            "web_search",
            crate::WEB_EXTRACT_TOOL,
            crate::SANDBOX_READ_DELEGATED_FILE_TOOL,
            "read_file",
            "list_dir",
            "write_file",
            "request_folder_access",
            "connect_folder",
            "list_connected_folders",
            "list_folder",
            "read_connected_file",
            "import_connected_file",
            crate::WRITE_OUTPUT_TO_CONNECTED_FOLDER_TOOL,
            "spawn_sandbox_agent",
            "wait_for_agents",
            crate::ASK_USER_QUESTIONS_TOOL,
            crate::EXIT_PLAN_MODE_TOOL,
            crate::UPDATE_TASK_PLAN_TOOL,
            "exec",
            crate::local_app::CREATE_APP_TOOL,
        ] {
            let folded = RendererToolName::from(name);
            assert_ne!(
                folded,
                RendererToolName::Other,
                "{name} folded to Other, so it would render as a generic tool"
            );
            assert_eq!(
                serde_json::to_value(folded).unwrap(),
                serde_json::json!(name),
                "{name} does not serialize back to its own wire spelling"
            );
        }
    }

    #[test]
    fn an_unregistered_name_folds_to_other() {
        for name in [
            "mcp__vendor__exfiltrate",
            "private_read_variant",
            "",
            "SEARCH",
            "other",
            // Removed 2026-07: journals recorded before the files-first outputs
            // migration still carry these calls and must render as generic
            // tools rather than fail the fold.
            "create_deliverable",
        ] {
            assert_eq!(RendererToolName::from(name), RendererToolName::Other);
        }
    }
}
