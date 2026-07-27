// Generated from Rust. Do not edit.
//
// Regenerate: UPDATE_WIRE_TYPES=1 cargo test -p openwave-server
//
// These describe the JSON the server actually sends, derived from the same
// serde attributes serde itself reads. They are the input to the runtime
// validators in api.ts, not a replacement for them: the validators enforce
// bounds, reject control characters, and cross-check server policy, none of
// which a type can express. See docs/wire-types.md.

/**
 * The approval policy class a tool declares for itself.
 *
 * Policy maps class → auto-approve / ask / deny. In v1: `ReadOnly` and
 * `Workspace` auto-approve; `Sensitive` parks on the approval gate unless a
 * matching standing grant covers the call.
 */
export type ApprovalClass = "read_only" | "workspace" | "sensitive";

/**
 * Identifies one tool call, stable across its request/approval/result.
 */
export type CallId = string;

/**
 * Identifies a persisted message within a chat.
 */
export type MessageId = string;

export type RendererAgentEvent = { "type": "turn_started", turn_id: TurnId, } | { "type": "text_delta", text: string, } | { "type": "reasoning_delta" } | { "type": "stream_interrupted" } | { "type": "tool_call_started", call_id: CallId, name: RendererToolName, } | { "type": "tool_call_args_delta", call_id: CallId, } | { "type": "user_questions_asked", call_id: CallId, turn_id: TurnId, } | { "type": "approval_required", call_id: CallId, action: RendererToolName, approval: ToolApprovalKind, class: ApprovalClass, 
/**
 * The one deliberate opening in this boundary. A human cannot consent
 * to a command they are not shown, so a tool may project a closed,
 * field-by-field view of the action under review. Tools without one
 * send nothing, as every tool did before.
 */
preview?: ToolActionPreview, } | { "type": "approval_decided", call_id: CallId, approved: boolean, } | { "type": "tool_call_completed", call_id: CallId, status: RendererToolStatus, 
/**
 * What the call did, when its tool projects it. Approval is not the
 * only moment a person needs to see the action.
 */
action?: ToolActionPreview, 
/**
 * What the call produced. A command's output is the reason it ran;
 * withholding it leaves the transcript asserting that something
 * happened without ever showing what.
 */
result?: ToolResultPreview, } | { "type": "turn_completed" } | { "type": "turn_failed" } | { "type": "turn_cancelled" } | { "type": "user_steered", message_id: MessageId, text: string, } | { "type": "context_truncated" } | { "type": "event_omitted" };

export type RendererSequencedEvent = { seq: number, event: RendererAgentEvent, };

/**
 * Tool names are model-controlled. Only names with fixed renderer
 * presentations cross the boundary; everything else becomes `other`.
 *
 * This enum is the renderer's tool vocabulary. The desktop's union, its
 * runtime guard, its copy table, and its icon table all derive from the
 * TypeScript generated here, so adding a variant cannot leave one of them
 * behind — see `docs/wire-types.md`.
 */
export type RendererToolName = "search" | "list_sources" | "read_source" | "read_tool_result" | "web_search" | "read_delegated_file" | "read_file" | "list_dir" | "write_file" | "create_deliverable" | "request_folder_access" | "connect_folder" | "list_connected_folders" | "list_folder" | "read_connected_file" | "import_connected_file" | "spawn_sandbox_agent" | "wait_for_agents" | "ask_user_questions" | "exec" | "other";

export type RendererToolStatus = "completed" | "failed";

/**
 * The action a call will take, in a form a human can inspect.
 *
 * Approval cards need this because consent to an action you cannot see is not
 * consent. Result cards reuse it so the same action is described the same way
 * before and after it runs.
 */
export type ToolActionPreview = { "tool": "exec", 
/**
 * Executable name or path the model chose.
 */
command: string, 
/**
 * Arguments passed directly to the executable.
 */
args: Array<string>, 
/**
 * Working directory relative to the chat's private scratch, never a
 * host path.
 */
cwd: string, } | { "tool": "search", query: string, } | { "tool": "web_search", query: string, };

/**
 * Closed immutable consent semantics stored with each approval request.
 *
 * Each presentable variant names the egress a human is consenting to, so the
 * renderer can describe the action without ever seeing the model-authored
 * summary or arguments. `Unsupported` is the fail-closed default: a Sensitive
 * action the server can only reject, never approve.
 */
export type ToolApprovalKind = "search_may_share_query_and_excerpts" | "web_search_may_share_query" | "exec_may_run_networked_command" | "external_mcp_may_call_server" | "unsupported";

/**
 * What a call produced, in a form a human can read.
 *
 * A command's output is the whole reason to run it. Withholding it leaves the
 * transcript asserting that something happened without ever showing what.
 */
export type ToolResultPreview = { "tool": "exec", 
/**
 * Process exit status, or `None` when it was killed by a signal.
 */
exit_code: number | null, 
/**
 * Whether the provider stopped the command at its time limit.
 */
timed_out: boolean, 
/**
 * Whether the provider dropped output past its capture limit.
 */
output_truncated: boolean, stdout: string, stderr: string, };

/**
 * Identifies one turn: a single user input through to the final answer.
 */
export type TurnId = string;

/**
 * Every tool name the renderer will accept, at runtime.
 *
 * An allowlist, not a display transformation. Tool events come from
 * providers, so a name outside this set must never reach a card, an icon,
 * or a copy table. The server folds anything unrecognized to `other`.
 *
 * Emitted from the same enum as `RendererToolName` above, so the runtime
 * list and the type cannot disagree.
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
