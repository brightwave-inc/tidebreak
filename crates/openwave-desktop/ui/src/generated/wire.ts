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
 * Fixed, renderer-safe names for supported live work.
 *
 * Adding a durable tool does not automatically expose it to a renderer: it
 * must be deliberately admitted here with a safe label.
 */
export type AgentActivityKind = "web_search" | "read_delegated_file" | "list_connected_folders" | "list_folder" | "read_connected_file" | "import_connected_file";

/**
 * Renderer-safe projection of one live supported checkpoint.
 */
export type AgentActivitySnapshot = { kind: AgentActivityKind, status: AgentActivityStatus, };

/**
 * Coarse checkpoint lifecycle suitable for display.
 *
 * This intentionally does not mirror all durable executor states; only live
 * work is represented, and terminal checkpoints produce no activity.
 */
export type AgentActivityStatus = "waiting" | "running";

/**
 * Closed renderer-safe acknowledgement for sandbox cancellation.
 */
export type AgentRunCancellationSnapshot = { id: AgentRunId, status: AgentRunCancellationStatus, };

export type AgentRunCancellationStatus = "cancelling" | "cancelled";

/**
 * Execution boundary for an [`AgentRun`].
 */
export type AgentRunExecution = "foreground" | "sandbox";

/**
 * Identifies one durable foreground or sandboxed background agent run.
 */
export type AgentRunId = string;

/**
 * Renderer-safe state for one agent run.
 *
 * Worker lease tokens, delegated inputs, scheduling budgets, and other
 * executor-facing fields intentionally remain inside the server/store boundary.
 */
export type AgentRunSnapshot = { id: AgentRunId, parent_id: AgentRunId | null, execution: AgentRunExecution, status: AgentRunStatus, started_at: string | null, finished_at: string | null, 
/**
 * Stable, bounded classification suitable for renderer display.
 */
last_error_code: string | null, 
/**
 * The currently checkpointed, renderer-safe activity, if any.
 *
 * This is intentionally a small fixed vocabulary. It never exposes tool
 * arguments, results, provider call identities, executor leases, or raw
 * executor diagnostics.
 */
activity: AgentActivitySnapshot | null, created_at: string, updated_at: string, spawn_call_id: CallId | null, };

/**
 * Durable lifecycle of an [`AgentRun`].
 */
export type AgentRunStatus = "active" | "queued" | "running" | "cancelling" | "waiting" | "retry_wait" | "completed" | "failed" | "cancelled";

/**
 * The approval policy class a tool declares for itself.
 *
 * Policy maps class → auto-approve / ask / deny. In v1: `ReadOnly` and
 * `Workspace` auto-approve; `Sensitive` parks on the approval gate unless a
 * matching standing grant covers the call.
 */
export type ApprovalClass = "read_only" | "workspace" | "sensitive";

/**
 * Opaque renderer identity for one assistant-message citation.
 */
export type AssistantCitationId = string;

/**
 * Renderer-safe historical citation projected from immutable evidence.
 *
 * The source identity and canonical-text span travel with the excerpt because
 * a citation is a position in a document, not just a quotation of one: without
 * them a reader can only be shown the words again, never where they came from.
 * Neither is a capability — the document id already addresses the source panel,
 * and the span only means anything against text the same client can already
 * read.
 */
export type AssistantCitationSnapshot = { id: AssistantCitationId, ordinal: number, 
/**
 * The cited source, addressable as a document panel.
 */
document_id: DocumentId, 
/**
 * Half-open byte range of the cited passage in that document's canonical
 * text, which is the text the extracted-text view renders.
 */
span: CitationSpan, excerpt: string, heading: string | null, pages: Array<number>, };

/**
 * Identifies one tool call, stable across its request/approval/result.
 */
export type CallId = string;

/**
 * A persistent conversation with an exact, ordered host-root projection.
 */
export type Chat = { 
/**
 * Stable identifier.
 */
id: ChatId, 
/**
 * The project this chat belongs to, or `None` for a loose (projectless) chat.
 */
project_id: ProjectId | null, 
/**
 * Human-facing title; `None` until one is set or derived.
 */
title: string | null, 
/**
 * The model this chat runs against, or `None` to use the configured default.
 */
model: string | null, 
/**
 * Reasoning-effort override for this chat, honored only by models that
 * expose the control; `None` leaves the provider's default in force.
 */
reasoning_effort: ReasoningEffort | null, 
/**
 * CAS revision of this conversation's exact root projection.
 */
attachment_revision: number, 
/**
 * Ordered opaque roots available for future broker-backed operations.
 * Live broker authorization remains mandatory and may revoke access at any
 * time, regardless of this projection.
 */
root_attachments: Array<ChatRootAttachment>, 
/**
 * When the chat was created.
 */
created_at: string, };

/**
 * Identifies a persistent conversation.
 */
export type ChatId = string;

/**
 * A renderer-safe durable transcript entry. Internal routing and tool state
 * deliberately remain behind the server boundary.
 */
export type ChatMessageSnapshot = { id: MessageId, role: TranscriptRole, content: string, created_at: string, citations: Array<AssistantCitationSnapshot>, 
/**
 * Images submitted with this user message. These are durable identity and
 * geometry only; image bytes remain behind a chat-scoped authenticated
 * endpoint and never enter the transcript payload.
 */
image_attachments?: Array<TranscriptImageAttachment>, refusal?: RendererRefusal, };

/**
 * One pathless root in a conversation's exact ordered projection.
 */
export type ChatRootAttachment = { 
/**
 * Opaque broker root identity. This value grants no authority by itself.
 */
root_id: HostRootId, 
/**
 * Product-level provenance for ordering and future management UI.
 */
origin: RootAttachmentOrigin, };

/**
 * A completed tool invocation with no arbitrary result text, provider
 * metadata, executor identity, lease, or diagnostic detail. The only action or
 * result it can carry is one a tool explicitly projects through a closed type.
 */
export type ChatToolActivitySnapshot = { 
/**
 * Allowlisted renderer tool name, never a provider-supplied one.
 *
 * A name rather than display copy: the renderer already derives a live
 * call's wording from its name, and sending prose here made a copy change
 * silently break history hydration.
 *
 * Typed as the vocabulary rather than a string so the generated TypeScript
 * stays a union. As `&'static str` it generated as `string`, which compiles
 * on both sides while silently dropping the allowlist the renderer's copy
 * and icon tables are keyed on.
 */
tool: RendererToolName, 
/**
 * Closed projection of what the call did, when its tool has one. Rebuilt
 * from the arguments it ran with, so history describes the same action
 * the live stream did.
 */
action?: ToolActionPreview, 
/**
 * Closed projection of an actionable result. Arbitrary result text is
 * never included.
 */
result?: ToolResultPreview, background_agent_run_id?: AgentRunId, status: ChatToolActivityStatus, started_at: string, finished_at: string | null, };

/**
 * Fixed lifecycle vocabulary exposed for a historical tool card.
 */
export type ChatToolActivityStatus = "completed" | "failed" | "cancelled";

/**
 * One visible transcript plus the durable journal watermark that produced it.
 * The renderer uses the watermark to subscribe only to future events, avoiding
 * duplicate text when reopening a completed conversation.
 */
export type ChatTranscript = { messages: Array<ChatMessageSnapshot>, 
/**
 * Finished tool activity from terminal turns, projected through a fixed
 * renderer-safe allowlist. Canonical tool records never cross this API.
 */
tool_activity: Array<ChatToolActivitySnapshot>, last_event_seq: number, };

/**
 * A citation's byte range, projected for the renderer.
 *
 * [`crate::ByteSpan`] is `usize`, which is a host-width detail rather than part
 * of a wire contract; canonical text is bounded well inside `u32`.
 */
export type CitationSpan = { 
/**
 * Inclusive start byte offset.
 */
start: number, 
/**
 * Exclusive end byte offset.
 */
end: number, };

/**
 * Renderer-safe configuration and readiness.
 */
export type CodeExecutionConfigInfo = { provider?: CodeExecutionProviderKind, timeout_ms: number, available: boolean, has_credential: boolean, };

/**
 * Renderer-safe readiness for one managed provider's fixed credential slot.
 */
export type CodeExecutionCredentialReadiness = { provider: CodeExecutionProviderKind, has_credential: boolean, };

/**
 * A configured code-execution backend.
 */
export type CodeExecutionProviderKind = "local" | "e2b" | "daytona";

/**
 * Conservative, user-inspectable capabilities for one model served by an
 * OpenAI-compatible endpoint.
 */
export type CustomModelConfig = { 
/**
 * Exact model id sent to the endpoint.
 */
id: string, 
/**
 * Optional human-facing label.
 */
display_name?: string | null, 
/**
 * Context limit used by OpenWave's reducer.
 */
context_window: number, 
/**
 * Maximum output sent to the endpoint.
 */
max_output_tokens: number, };

/**
 * Identifies an authoritative source document.
 *
 * Usually minted fresh with [`DocumentId::new`], but [`DocumentId::derive`]
 * preserves the existing stable URI identity used by retrieval ingestion.
 */
export type DocumentId = string;

/**
 * One entitled connected app, with the slugs of the MCP endpoints that
 * aggregate it — the `mcp:<slug>` resources a mount would request.
 */
export type GatewayAppInfo = { id: string, name: string, app_kind: string, enabled: boolean, mcp_endpoint_slugs: Array<string>, };

/**
 * Renderer-safe list of the connected apps the signed-in user is entitled
 * to, fetched live from the gateway (never cached: a revoked grant is gone
 * on the next request).
 */
export type GatewayApps = { 
/**
 * False when the connected gateway predates the JSON apps surface; the
 * renderer hides the section instead of showing an empty list as "none".
 */
supported: boolean, apps: Array<GatewayAppInfo>, };

/**
 * Renderer-safe projection of the gateway connection state. Never carries
 * token material — only what the settings surface displays.
 */
export type GatewayStatus = { configured: boolean, enabled: boolean, base_url?: string, signed_in: boolean, account_hint?: string, installation_id?: string, model_count: number, sign_in: SignInProgress, };

/**
 * Opaque identifier for a folder registered with a host broker.
 *
 * This is product projection data, not authority: possession of an id never
 * grants access to the corresponding host path. The broker independently
 * validates live attachments, consent, capabilities, and revocation.
 */
export type HostRootId = string;

/**
 * An input modality a model accepts.
 *
 * `snake_case` matches the strings `as_str` has always produced, so the enum
 * serializes exactly as the hand-built list of strings it replaces on the wire.
 */
export type InputModality = "text" | "image";

/**
 * Renderer-safe connection lifecycle.
 */
export type McpHealth = "initializing" | "healthy" | "degraded" | "reconnecting" | "disabled";

/**
 * One external MCP server definition: a local stdio process (`command`) or a
 * remote Streamable HTTP endpoint (`url`). Exactly one of the two is set;
 * [`validate_servers`] enforces that process fields stay with `command` and
 * `bearer_token_env` stays with `url`.
 */
export type McpServerDefinition = { name: string, command: string | null, args: Array<string>, 
/**
 * Explicit literal values. The UI labels these as non-secret.
 */
env: { [key in string]: string }, 
/**
 * Parent environment names to forward. Their values never enter this type.
 */
env_from: Array<string>, cwd: string | null, 
/**
 * Streamable HTTP endpoint for a remote server.
 */
url: string | null, 
/**
 * Parent environment name holding the HTTP bearer token. The value is
 * resolved at connect time and never enters this type.
 */
bearer_token_env: string | null, request_timeout_ms: number, enabled: boolean, };

/**
 * One renderer-safe server projection. Resolved `env_from` values and child
 * process details are intentionally absent.
 */
export type McpServerInfo = { health: McpHealth, tool_count: number, diagnostic: string | null, name: string, command: string | null, args: Array<string>, 
/**
 * Explicit literal values. The UI labels these as non-secret.
 */
env: { [key in string]: string }, 
/**
 * Parent environment names to forward. Their values never enter this type.
 */
env_from: Array<string>, cwd: string | null, 
/**
 * Streamable HTTP endpoint for a remote server.
 */
url: string | null, 
/**
 * Parent environment name holding the HTTP bearer token. The value is
 * resolved at connect time and never enters this type.
 */
bearer_token_env: string | null, request_timeout_ms: number, enabled: boolean, };

export type McpServersInfo = { servers: Array<McpServerInfo>, };

/**
 * Where the sandboxed iframe should load one view from, valid once.
 */
export type McpViewSession = { frame_path: string, };

/**
 * Identifies a persisted message within a chat.
 */
export type MessageId = string;

/**
 * A selectable model in the catalog.
 */
export type ModelInfo = { 
/**
 * Stable provider-qualified selection key used by settings and chats.
 */
key: string, 
/**
 * The identifier passed to the provider and stored as `chat.model`.
 */
id: string, 
/**
 * Human-readable label for the selector (e.g. `"Claude Opus 4.8"`).
 */
display_name: string, 
/**
 * The provider that serves the model.
 */
provider: ProviderKind, 
/**
 * Whether the provider is enabled, configured, and credentialed.
 */
available: boolean, 
/**
 * Approximate context window in tokens.
 */
context_window: number, 
/**
 * Maximum model output in tokens.
 */
max_output_tokens: number, 
/**
 * Input modalities accepted by the model.
 */
input_modalities: Array<InputModality>, 
/**
 * Whether the model can produce an internal reasoning stream.
 */
supports_reasoning: boolean, 
/**
 * The reasoning-effort levels this model accepts, ascending. Empty when
 * the model exposes no effort control, which is what a client checks
 * before offering the selector at all.
 *
 * Carries the enum rather than plain strings so the generated TypeScript
 * is the same union a chat's stored effort has, and a client cannot offer
 * a level it could not then set.
 */
reasoning_efforts: Array<ReasoningEffort>, 
/**
 * Whether the model accepts image input alongside text.
 */
multimodal: boolean, };

/**
 * The roles the product resolves a model for.
 *
 * `#[non_exhaustive]` so a new role can land without breaking wire clients that
 * match on the string form.
 */
export type ModelRole = "chat" | "utility";

/**
 * One named model role and what it resolves to right now.
 */
export type ModelRoleInfo = { 
/**
 * The role this row describes.
 */
role: ModelRole, 
/**
 * The catalog key the user selected for this role, or `None` when the role
 * is left automatic.
 */
selection: string | null, 
/**
 * The catalog key this role resolves to right now, selection or not.
 *
 * A selector that offers "automatic" as a choice can only say what that
 * choice means if the server says which model it lands on. `None` when the
 * role resolves to nothing the catalog can name, which leaves the client
 * with nothing to promise rather than a guess — and, for `utility`, means
 * the work that depends on it is skipped.
 */
resolved_key: string | null, };

/**
 * Closed renderer-safe pending approval projection. Canonical arguments,
 * model-authored summaries, and unknown tool names never cross this boundary;
 * only a tool's own closed preview of the action under review does.
 */
export type PendingApprovalSnapshot = { call_id: CallId, turn_id: TurnId, action: RendererToolName, approval: ToolApprovalKind, class: ApprovalClass, 
/**
 * Absent, not null, when the tool projects no action.
 */
preview?: ToolActionPreview, can_approve: boolean, can_remember: boolean, };

/**
 * One folder-access request that is safe for an untrusted renderer to present.
 *
 * This intentionally omits the canonical tool name and arguments, chat and
 * executor identities, provider metadata, lifecycle details, and diagnostics.
 */
export type PendingFolderAccessRequest = { call_id: CallId, turn_id: TurnId, reason: string, folder_hint: RequestedFolderHint | null, claimed: boolean, };

/**
 * Renderer-safe replacement approval. Canonical output, root, and destination
 * identities remain native-only; the card can approve or decline this exact call.
 */
export type PendingOutputWritebackRequest = { call_id: CallId, turn_id: TurnId, claimed: boolean, };

/**
 * Renderer-safe, durable card projection.
 *
 * It contains only the validated presentation contract. Provider metadata,
 * raw tool arguments, leases, executor identities, and diagnostics stay
 * behind the server boundary.
 */
export type PendingUserQuestions = { call_id: CallId, turn_id: TurnId, questions: Array<UserQuestion>, asked_at: string, };

/**
 * An optional grouping of chats that share project context and a document
 * corpus. A chat may belong to a project or stand alone — unlike some designs
 * that make a project mandatory, OpenWave keeps loose, projectless chats.
 */
export type Project = { 
/**
 * Stable identifier.
 */
id: ProjectId, 
/**
 * Human-facing title.
 */
title: string | null, 
/**
 * CAS revision of the ordered root projection.
 */
attachment_revision: number, 
/**
 * Ordered opaque root defaults for conversations created in this project.
 * These ids are product state, never host authorization.
 */
root_attachments: Array<HostRootId>, 
/**
 * When the project was created.
 */
created_at: string, };

/**
 * Identifies a project: an optional grouping a chat may belong to.
 */
export type ProjectId = string;

/**
 * Public view of a provider — never includes the credential itself.
 */
export type ProviderInfo = { 
/**
 * Provider kind.
 */
kind: ProviderKind, 
/**
 * Whether the provider is enabled for routing.
 */
enabled: boolean, 
/**
 * Configured base URL, if any.
 */
base_url?: string, 
/**
 * Vertex AI location. Never includes the project id from the credential.
 */
vertex_location?: string, 
/**
 * Whether a credential is stored (never the credential itself).
 */
has_credential: boolean, 
/**
 * Explicit custom model entries for this endpoint.
 */
models: Array<CustomModelConfig>, };

/**
 * The known provider kinds. `#[non_exhaustive]` so new kinds can land without
 * breaking wire clients that match on the string form.
 */
export type ProviderKind = "anthropic" | "openai" | "gemini" | "openai_compatible" | "model_gateway";

/**
 * How hard a reasoning-capable model should think before answering.
 *
 * The scale runs from [`Self::None`] to [`Self::Max`] and the ordering is the
 * scale itself, not an implementation detail: comparisons and
 * [`Self::clamp_to`] rely on it.
 *
 * No model accepts the whole scale. `none` is an OpenAI level that the Claude
 * family rejects, `max` is missing from several rows on both routes, and some
 * models take no effort control at all. A model's accepted levels live on its
 * registry entry; a stored choice is mapped onto them with [`Self::clamp_to`]
 * before a request is built.
 *
 * Persisted per chat as the token from [`Self::as_str`] and threaded into the
 * model request for each turn.
 */
export type ReasoningEffort = "none" | "low" | "medium" | "high" | "xhigh" | "max";

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
result?: ToolResultPreview, } | { "type": "turn_completed" } | { "type": "turn_refused", refusal: RendererRefusal, } | { "type": "turn_failed" } | { "type": "turn_cancelled" } | { "type": "user_steered", message_id: MessageId, text: string, } | { "type": "context_truncated" } | { "type": "event_omitted" };

/**
 * Bounded refusal metadata safe to present in the desktop transcript.
 */
export type RendererRefusal = { category: string | null, partial_output: boolean, };

export type RendererSequencedEvent = { seq: number, event: RendererAgentEvent, };

/**
 * A tool name the renderer is allowed to present.
 *
 * The desktop's union, its runtime guard, its copy table, and its icon table
 * are all generated from this enum, so a variant added here cannot leave one of
 * them behind — see `docs/wire-types.md`.
 */
export type RendererToolName = "search" | "list_sources" | "read_source" | "read_tool_result" | "web_search" | "read_delegated_file" | "read_file" | "list_dir" | "write_file" | "create_deliverable" | "request_folder_access" | "connect_folder" | "list_connected_folders" | "list_folder" | "read_connected_file" | "import_connected_file" | "write_output_to_connected_folder" | "spawn_sandbox_agent" | "wait_for_agents" | "ask_user_questions" | "exec" | "other";

export type RendererToolStatus = "completed" | "failed";

/**
 * Non-authoritative, well-known starting location for the native picker.
 *
 * This is deliberately not a free-form path. The trusted desktop decides how
 * (or whether) to map it to a local picker location.
 */
export type RequestedFolderHint = "documents" | "downloads";

/**
 * Why a root appears in one conversation's exact ordered projection.
 */
export type RootAttachmentOrigin = "project_default" | "conversation";

/**
 * Runtime settings a client can read. The API key itself is never returned —
 * it lives in the `SecretProvider`, not the store — only whether one is set.
 */
export type Settings = { 
/**
 * The model turns run against, or `None` to use the server's default.
 */
model: string | null, 
/**
 * Whether a model API key is configured (never the key itself).
 */
has_api_key: boolean, };

/**
 * Renderer-safe progress of the current sign-in attempt.
 */
export type SignInProgress = { "state": "idle" } | { "state": "pending", authorization_url: string, } | { "state": "failed", message: string, };

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
cwd: string, } | { "tool": "search", query: string, } | { "tool": "web_search", query: string, 
/**
 * Sites the search is confined to, empty when the model named none.
 */
domains: Array<string>, 
/**
 * Earliest publication date the search will accept, as the model
 * wrote it. Kept verbatim rather than reformatted: the card's job is
 * to show what the provider is actually told.
 */
start_published_at: string | null, 
/**
 * Latest publication date the search will accept.
 */
end_published_at: string | null, };

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
output_truncated: boolean, stdout: string, stderr: string, } | { "tool": "web_search_provider_required" } | { "tool": "mcp_app", 
/**
 * The configured MCP server namespace that serves the view.
 */
server: string, 
/**
 * The validated `ui://` document reference.
 */
resource_uri: string, };

/**
 * One renderer-safe image identity attached to a historical user message.
 */
export type TranscriptImageAttachment = { 
/**
 * Content-addressed opaque attachment identity, not a host path.
 */
attachment_id: string, 
/**
 * Sniffed IANA media type from the trusted image ingest boundary.
 */
media_type: string, 
/**
 * Header-derived dimensions, bounded at image publication.
 */
width: number, height: number, };

/**
 * The roles a visible transcript entry can have.
 *
 * Narrower than [`Role`] on purpose. The transcript shows the conversation, not
 * the model's plumbing, so `System` and `Tool` never appear — and that was
 * previously guaranteed only by a `matches!` filter at the one call site, while
 * the snapshot's own type still admitted all four. The renderer mirrored the
 * narrow version and branched on `assistant` with no third arm, so a `system`
 * entry reaching it would have rendered as a user message.
 *
 * Encoding it here makes the guarantee the type's rather than the caller's, and
 * makes a new [`Role`] variant a decision in [`Self::for_transcript`] instead of
 * something that silently appears in the transcript.
 */
export type TranscriptRole = "user" | "assistant";

/**
 * Identifies one turn: a single user input through to the final answer.
 */
export type TurnId = string;

/**
 * One bounded question shown to the user.
 */
export type UserQuestion = { id: string, header: string, question: string, options: Array<UserQuestionOption>, allow_free_form: boolean, };

/**
 * One mutually exclusive answer choice.
 */
export type UserQuestionOption = { id: string, label: string, description: string, };

/**
 * Public state returned by the local API. It intentionally reports only
 * selection and credential presence — key material never crosses the secret
 * boundary.
 */
export type WebSearchConfigInfo = { provider?: WebSearchProviderKind, timeout_ms: number, has_credential: boolean, };

/**
 * Credential readiness for one fixed web-search provider. This public shape
 * deliberately carries no secret material.
 */
export type WebSearchCredentialReadiness = { provider: WebSearchProviderKind, has_credential: boolean, };

/**
 * A configured web-search backend. The stable string also selects its secret
 * reference; it is intentionally not a model-controlled argument.
 */
export type WebSearchProviderKind = "exa" | "tavily";

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
  "write_output_to_connected_folder",
  "spawn_sandbox_agent",
  "wait_for_agents",
  "ask_user_questions",
  "exec",
  "other",
] as const;
