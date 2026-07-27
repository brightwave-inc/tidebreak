// Generated from Rust. Do not edit.
//
// Source: `RendererToolName` in crates/openwave-server/src/event_projection.rs
// Regenerate: UPDATE_WIRE_TYPES=1 cargo test -p openwave-server
//
// The runtime list and the type are emitted together so a tool cannot
// exist in one and not the other. See docs/wire-types.md.

/**
 * Every tool name the renderer will accept.
 *
 * This is an allowlist, not a display transformation. Tool events come from
 * providers, so a name outside this set must never reach a card, an icon, or
 * a copy table. The server folds anything unrecognized to `other`.
 */
export const RENDERER_TOOL_NAMES = [
  "search",
  "list_sources",
  "read_source",
  "read_tool_result",
  "web_search",
  "read_delegated_file",
  "read_file",
  "list_dir",
  "write_file",
  "create_deliverable",
  "request_folder_access",
  "connect_folder",
  "list_connected_folders",
  "list_folder",
  "read_connected_file",
  "import_connected_file",
  "spawn_sandbox_agent",
  "wait_for_agents",
  "ask_user_questions",
  "exec",
  "other",
] as const;

export type RendererToolName = (typeof RENDERER_TOOL_NAMES)[number];
