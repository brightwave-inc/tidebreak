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
 * One renderer-safe entry in a background run's ordered activity history.
 *
 * Built from durable sandbox tool calls, but it carries only the fixed
 * [`AgentActivityKind`] vocabulary, a coarse lifecycle, and a timestamp. Tool
 * arguments, queries, results, folder and file identities, host paths,
 * provider identities, executor leases, and raw diagnostics all remain
 * server-side, exactly as they do for the live `activity` projection.
 */
export type AgentActivityHistoryItem = { kind: AgentActivityKind, outcome: AgentActivityOutcome, at: string, };

/**
 * Fixed, renderer-safe names for supported live work.
 *
 * Adding a durable tool does not automatically expose it to a renderer: it
 * must be deliberately admitted here with a safe label.
 */
export type AgentActivityKind = "web_search" | "read_delegated_file" | "list_connected_folders" | "list_folder" | "read_connected_file" | "import_connected_file";

/**
 * Coarse, renderer-safe lifecycle for one historical activity entry.
 *
 * Unlike [`AgentActivityStatus`], which only names live work, this also
 * admits the three terminal outcomes so a settled step can be shown in an
 * ordered timeline. It carries no failure detail: a failed step is only
 * "failed", never why.
 */
export type AgentActivityOutcome = "waiting" | "running" | "completed" | "failed" | "cancelled";

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
 * Where an [`AgentRun`]'s loop executes.
 *
 * Every run executes inside the OpenWave server process today. A run
 * executing inside an execution provider's boundary adds a variant here
 * rather than a second meaning to [`AgentRunTier`].
 */
export type AgentRunExecutionLocation = "in_process" | "container";

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
export type AgentRunSnapshot = { id: AgentRunId, parent_id: AgentRunId | null, tier: AgentRunTier, execution_location: AgentRunExecutionLocation, status: AgentRunStatus, started_at: string | null, finished_at: string | null, 
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
activity: AgentActivitySnapshot | null, 
/**
 * Whether a completed background run committed an immutable terminal
 * receipt, which happens atomically with its terminal state.
 *
 * This is presence only: the payload, its display text, and any merged
 * deliverable never cross this boundary. A renderer uses it to offer a
 * "view output" affordance and link to the outputs surface.
 */
produced_output: boolean, created_at: string, updated_at: string, spawn_call_id: CallId | null, };

/**
 * Durable lifecycle of an [`AgentRun`].
 */
export type AgentRunStatus = "active" | "queued" | "running" | "cancelling" | "waiting" | "retry_wait" | "completed" | "failed" | "cancelled";

/**
 * Run tier of an [`AgentRun`]: who advances the run.
 *
 * Formerly one half of `AgentRunExecution` (`foreground | sandbox`), which
 * fused this axis with [`AgentRunExecutionLocation`]. The two agreed only
 * while every run executed in-process, so the field split before a second
 * location could exist.
 */
export type AgentRunTier = "foreground" | "background";

/**
 * The typed refusal body — the `{ kind, message }` shape every route error
 * carries, with the kind closed so a client never string-matches prose.
 */
export type AppInvokeRefusal = { kind: AppInvokeRefusalKind, message: string, };

/**
 * The stable machine-readable refusals of `POST /apps/{id}/invoke`.
 *
 * Unlike the passthrough payloads, the refusal envelope is host-authored and
 * the renderer must branch on it — `consent_required` is the arm that will
 * open the grant sheet — so the kind is a closed generated union rather than
 * a free-form string.
 */
export type AppInvokeRefusalKind = "app_not_found" | "not_pinned" | "consent_required" | "unknown_tool";

/**
 * Where the sandboxed iframe should load one app revision from, valid once.
 */
export type AppViewSession = { frame_path: string, };

/**
 * The approval policy class a tool declares for itself.
 *
 * Policy maps class → auto-approve / ask / deny. In v1: `ReadOnly` and
 * `Workspace` auto-approve; `Sensitive` parks on the approval gate unless a
 * matching standing grant covers the call.
 */
export type ApprovalClass = "read_only" | "workspace" | "sensitive";

/**
 * How wide a standing grant the human chose, narrowest first.
 *
 * The renderer names a rung; the server builds the concrete grant from the
 * arguments the call is parked on. A grant can therefore only ever describe
 * the action that was actually under review.
 */
export type ApprovalGrantRung = "exact_action" | { "command_prefix": { tokens: number, } } | "whole_tool";

/**
 * Opaque renderer identity for one assistant-message citation.
 */
export type AssistantCitationId = string;

/**
 * Renderer-safe historical citation stored beside an assistant message.
 */
export type AssistantCitationSnapshot = { id: AssistantCitationId, ordinal: number, document_id: DocumentId, locator: CitationLocator, };

/**
 * Where the Auto-mode judge stands on one parked call.
 *
 * The marker is load-bearing for the renderer: without it, "the judge is
 * still deciding" and "the judge declined, a human is needed" are both just
 * `Pending`, indistinguishable except by waiting.
 */
export type AutoJudgeStatus = "judging" | "approved" | "declined";

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
 * How much this chat lets the agent do between approvals; `None` means
 * [`PermissionMode::Ask`].
 */
permission_mode: PermissionMode | null, 
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
image_attachments?: Array<TranscriptImageAttachment>, 
/**
 * Files submitted with this user message. Their bytes remain behind the
 * existing chat-scoped document endpoints.
 */
file_attachments?: Array<TranscriptFileAttachment>, };

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
 * One terminal turn's renderer-safe status and visible streamed content.
 *
 * A completed turn points at its authoritative assistant message. Failed and
 * cancelled turns have no message, but remain first-class transcript entries
 * carrying the partial prose and reasoning the reader already saw live.
 */
export type ChatTerminalTurnSnapshot = { turn_id: TurnId, message_id?: MessageId, status: ChatTerminalTurnStatus, partial_content: string, reasoning?: string, refusal?: RendererRefusal, failure_category?: TurnFailureCategory, finished_at: string, };

export type ChatTerminalTurnStatus = "completed" | "failed" | "cancelled";

/**
 * A completed tool invocation with no arbitrary result text, provider
 * metadata, executor identity, lease, or diagnostic detail. The only action or
 * result it can carry is one a tool explicitly projects through a closed type.
 */
export type ChatToolActivitySnapshot = { 
/**
 * Canonical call id, the same [`crate::id::CallId`] the live event stream
 * carried for this call.
 *
 * History withholds arbitrary call detail, but not this identity: the MCP
 * App payload route already keys renderer-readable data on exactly this id
 * for the same authenticated client, and a rehydrated app view must present
 * it to resolve its payload. Without it, history cards invented a local id
 * and every replayed app view fetched a payload the server could only
 * reject.
 */
call_id: CallId, 
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
result?: ToolResultPreview, 
/**
 * Set when this call retained a projection that no longer deserializes.
 *
 * The projection is a closed union that is allowed to move, and rows
 * written before a change may no longer parse against it. Distinguishing
 * that from "this call projected nothing" is the difference between a card
 * that says its result can no longer be shown and one that silently
 * vanishes — which would read as the call never having produced anything.
 *
 * A property of reading storage, not of the result: the live stream builds
 * its projection in memory and can never set this.
 */
result_unreadable: boolean, background_agent_run_id?: AgentRunId, status: ChatToolActivityStatus, started_at: string, finished_at: string | null, };

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
tool_activity: Array<ChatToolActivitySnapshot>, 
/**
 * Status and streamed presentation for every terminal turn. This owns
 * terminal metadata even when no assistant message was committed.
 */
terminal_turns: Array<ChatTerminalTurnSnapshot>, last_event_seq: number, };

/**
 * A small, human-scale position inside a cited document.
 *
 * Validation is intentionally loose. A page or line that does not exist still
 * renders and opens the document as close to that position as the reader can.
 */
export type CitationLocator = { "kind": "document" } | { "kind": "page", page: number, } | { "kind": "pages", start: number, end: number, } | { "kind": "lines", start: number, end: number, } | { "kind": "sheet", sheet: string, cells: string | null, };

/**
 * Renderer-safe configuration and readiness.
 */
export type CodeExecutionConfigInfo = { provider?: CodeExecutionProviderKind, timeout_ms: number, available: boolean, has_credential: boolean, 
/**
 * The configured egress policy and each managed provider's enforcement
 * status, so the renderer can present the policy and disclose which
 * providers actually restrict egress today.
 */
egress: CodeExecutionEgressInfo, };

/**
 * Renderer-safe readiness for one managed provider's fixed credential slot.
 */
export type CodeExecutionCredentialReadiness = { provider: CodeExecutionProviderKind, has_credential: boolean, };

/**
 * A managed provider's egress-enforcement status, as host knowledge rather
 * than a claim the backend makes about itself.
 */
export type CodeExecutionEgressEnforcement = { provider: CodeExecutionProviderKind, status: EgressEnforcementStatus, 
/**
 * Destinations the vendor's mechanism keeps reachable regardless of the
 * configured policy — each a short purpose string straight from the
 * enforcement model, so the settings surface can show the caveat inline
 * instead of burying it in prose the user skims past.
 */
gaps: Array<string>, 
/**
 * A precondition the boundary is gated on that the host cannot verify
 * statically ("Daytona org tier 3+"). Present only for a
 * [`EgressEnforcementStatus::ConditionalBoundary`], so the surface can
 * state the condition inline rather than implying an unconditional
 * boundary.
 */
requirement?: string, };

/**
 * Renderer-safe egress policy plus per-provider enforcement disclosure.
 */
export type CodeExecutionEgressInfo = { 
/**
 * The configured host policy. `Open` is the default: managed sandboxes are
 * created with open internet access. An allowlist restricts every managed
 * sandbox created afterwards.
 */
policy: EgressConfig, 
/**
 * One row per managed provider, stating whether its egress restriction is
 * confirmed against the live vendor API or still pending confirmation.
 */
enforcement: Array<CodeExecutionEgressEnforcement>, };

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
 * preserves the existing stable URI identity used by source ingestion.
 */
export type DocumentId = string;

/**
 * Host-owned, non-secret egress policy for the managed exec sandboxes.
 *
 * The model never sets this (invariant 1): it is host configuration, carries
 * no secret, and accepts no endpoint. `Open` is the default and preserves
 * exec's out-of-the-box behavior — E2B and Daytona are created with open
 * internet access, as they always have been. Egress restriction is opt-in:
 * an `Allowlist` switches every managed sandbox created afterwards to
 * deny-by-default and compiles the listed domain patterns and CIDR blocks
 * into the vendor's per-sandbox network controls. An empty allowlist denies
 * everything on both axes.
 *
 * The strings are validated to the same [`DomainPattern`] and [`CidrBlock`]
 * grammar the decision layer uses, so a malformed grant is rejected at
 * `PUT` time rather than silently widening egress at sandbox creation.
 */
export type EgressConfig = { "mode": "open" } | { "mode": "allowlist", domains: Array<string>, cidrs: Array<string>, };

/**
 * The honest state of a managed provider's egress enforcement.
 *
 * Derived from the shipped enforcement model, never asserted per provider, so
 * the settings surface and the decision layer cannot disagree: if the model
 * says a vendor's mechanism leaves a general-purpose destination reachable,
 * the surface must not present it as a full boundary.
 */
export type EgressEnforcementStatus = "boundary" | "conditional_boundary" | "applied_with_gaps" | "unconfirmed";

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
 * token material — only what the settings surface displays. `base_url` is
 * the policy's gateway origin: present exactly when the profile is managed
 * with a usable URL (the retired `configured`/`enabled` bits collapsed into
 * its presence).
 */
export type GatewayStatus = { base_url?: string, signed_in: boolean, account_hint?: string, installation_id?: string, model_count: number, sign_in: SignInProgress, };

/**
 * How far a standing grant reaches.
 *
 * A grant used to cover exactly one chat, so "always allow `cargo`" was
 * re-asked in the next conversation and the one after it — the single
 * biggest source of prompting in the model. The level is chosen from where
 * the chat lives rather than put to the reader as a question: a chat in a
 * project grants across that project, and a loose chat has nothing wider to
 * mean, so it grants for itself. The card states which one it is about to
 * write; a grant nobody expected is the failure the ladder exists to prevent.
 */
export type GrantLevel = { "level": "chat", chat_id: ChatId, } | { "level": "project", project_id: ProjectId, };

/**
 * How much a standing grant covers.
 *
 * A grant is easy to widen by accident and hard to notice afterwards, so the
 * scope is stated in the grant itself rather than inferred from the tool. The
 * narrower variants exist because "don't ask me about commands again" is a
 * much larger thing to agree to than "don't ask me about `cargo` again".
 */
export type GrantScope = { "scope": "exact_action" } & ToolActionPreview | { "scope": "any_args_for", command: string, } | { "scope": "command_prefix", tokens: Array<string>, } | { "scope": "whole_tool" };

/**
 * Opaque identifier for a folder registered with a host broker.
 *
 * This is product projection data, not authority: possession of an id never
 * grants access to the corresponding host path. The broker independently
 * validates live attachments, consent, capabilities, and revocation.
 */
export type HostRootId = string;

/**
 * Image formats OpenWave will send to a provider.
 *
 * Deliberately closed. Every variant here is accepted by both the Anthropic
 * and OpenAI image APIs, so a value of this type can always be shaped for the
 * selected provider — adapters never have to reject a media type at send time.
 * Vector and exotic raster formats are excluded rather than passed through:
 * an unsupported type must fail at the trusted ingest boundary, where the user
 * can still act on it, not deep inside a turn.
 */
export type ImageMediaType = "png" | "jpeg" | "webp" | "gif";

/**
 * Durable identity of one image attachment.
 *
 * Everything here is safe to persist, log, and expose to a renderer. The blob
 * id is an opaque content-derived UUID, never a filesystem path, so it reveals
 * nothing about where the bytes live on disk.
 */
export type ImageRef = { 
/**
 * Content-addressed blob holding the pixels.
 */
blob_id: string, 
/**
 * Format the bytes were sniffed as at ingest.
 */
media_type: ImageMediaType, 
/**
 * Pixel width, read from the image header.
 */
width: number, 
/**
 * Pixel height, read from the image header.
 */
height: number, 
/**
 * Size of the stored bytes.
 */
byte_len: number, };

/**
 * An input modality a model accepts.
 *
 * `snake_case` matches the strings `as_str` has always produced, so the enum
 * serializes exactly as the hand-built list of strings it replaces on the wire.
 */
export type InputModality = "text" | "image";

/**
 * Renderer-safe resolved policy. Carries only what surfaces need to render
 * managed state: the verdict, the locked gateway URL, and its authority.
 */
export type ManagedPolicy = { managed: boolean, gateway_url?: string, source: ManagedPolicySource, 
/**
 * True when `source` asserted management but its gateway URL is missing,
 * unreadable, or invalid. The profile stays managed with no usable URL —
 * fail closed — and surfaces can name the authority that needs repair
 * instead of showing an opaque error.
 */
misconfigured: boolean, 
/**
 * A deep-link pairing awaiting the sign-in that is its consent. Runtime
 * state merged in by the `/policy` route from [`GatewayRuntime`]
 * (crate::gateway_runtime), never part of the durable resolution —
 * [`resolve`] always leaves it `None` — and only ever present while the
 * profile is unmanaged.
 */
pending_gateway_url?: string, };

/**
 * Which authority asserted the active policy.
 */
export type ManagedPolicySource = "os" | "provisioned" | "unmanaged";

/**
 * Renderer-safe connection lifecycle.
 */
export type McpHealth = "initializing" | "healthy" | "degraded" | "reconnecting" | "disabled";

/**
 * One external MCP server definition: a local stdio process (`command`), a
 * remote Streamable HTTP endpoint (`url`), or a gateway-managed endpoint
 * (`gateway_endpoint`). Exactly one of the three is set;
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
bearer_token_env: string | null, 
/**
 * Endpoint slug of a gateway MCP endpoint, mounted through the signed-in
 * model-gateway session. The endpoint URL and its short-lived bearer are
 * resolved from the session at every connection and never enter this
 * type.
 */
gateway_endpoint: string | null, request_timeout_ms: number, enabled: boolean, };

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
bearer_token_env: string | null, 
/**
 * Endpoint slug of a gateway MCP endpoint, mounted through the signed-in
 * model-gateway session. The endpoint URL and its short-lived bearer are
 * resolved from the session at every connection and never enter this
 * type.
 */
gateway_endpoint: string | null, request_timeout_ms: number, enabled: boolean, };

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
preview?: ToolActionPreview, can_approve: boolean, can_remember: boolean, 
/**
 * Complete standing-grant ladder for this exact call, narrowest first.
 *
 * Empty means only one-shot approval is available. The renderer receives
 * the whole ladder because command policy may refuse exact and whole-tool
 * grants as well as prefixes.
 */
grant_rungs: Array<ApprovalGrantRung>, 
/**
 * Where the Auto-mode judge stands, when one was engaged. Absent means
 * no judge ever owned this card.
 */
auto_judge_status?: AutoJudgeStatus, };

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
 * Renderer-safe, durable card projection of a proposed plan.
 */
export type PendingPlanApproval = { call_id: CallId, turn_id: TurnId, title: string, plan: string, proposed_at: string, };

/**
 * Renderer-safe, durable card projection.
 *
 * It contains only the validated presentation contract. Provider metadata,
 * raw tool arguments, leases, executor identities, and diagnostics stay
 * behind the server boundary.
 */
export type PendingUserQuestions = { call_id: CallId, turn_id: TurnId, questions: Array<UserQuestion>, asked_at: string, };

/**
 * How much a chat lets the agent do between approvals.
 *
 * The mode is the fallback, not the whole decision: a standing grant the
 * reader has already made covers its calls in every mode, and `ReadOnly`
 * tools never ask in any mode. The mode only decides what happens to a
 * mutating call that no grant covers — ask the reader, or proceed.
 *
 * Persisted per chat as the token from [`Self::as_str`] and read at turn
 * start, like the model selection: changing it mid-turn applies from the
 * next turn, and a reopened chat runs the way it ran before.
 */
export type PermissionMode = "plan" | "ask" | "auto" | "allow";

/**
 * The two decisions a reader can make about a proposed plan.
 */
export type PlanDecisionChoice = "accept" | "reject";

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

export type RendererAgentEvent = { "type": "turn_started", turn_id: TurnId, } | { "type": "text_delta", text: string, } | { "type": "reasoning_delta", text: string, } | { "type": "stream_interrupted" } | { "type": "tool_call_started", call_id: CallId, name: RendererToolName, } | { "type": "tool_call_args_delta", call_id: CallId, } | { "type": "user_questions_asked", call_id: CallId, turn_id: TurnId, } | { "type": "plan_proposed", call_id: CallId, turn_id: TurnId, } | { "type": "approval_required", call_id: CallId, action: RendererToolName, approval: ToolApprovalKind, class: ApprovalClass, 
/**
 * Whether the Auto-mode judge owns this card right now. The card
 * stays fully actionable either way; this only adds the "deciding
 * automatically" hint.
 */
auto_judging: boolean, 
/**
 * Complete standing-grant ladder for this exact call, narrowest first.
 * Empty means only one-shot approval is available.
 */
grant_rungs: Array<ApprovalGrantRung>, 
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
result?: ToolResultPreview, } | { "type": "turn_completed" } | { "type": "turn_refused", refusal: RendererRefusal, } | { "type": "turn_failed", 
/**
 * Why the turn failed, at the only resolution a client can act on.
 * The failure's `kind` and `message` stay internal.
 */
category: TurnFailureCategory, } | { "type": "turn_cancelled" } | { "type": "user_steered", message_id: MessageId, text: string, } | { "type": "context_truncated" } | { "type": "event_omitted" };

/**
 * One frame on a chat's event socket.
 *
 * Untagged, so a journaled event frame is byte-identical to what it has always
 * been: the sequence is the client's resume cursor and its dedup key, and every
 * consumer of it — replay, hydration, the session reducer — reads `seq` as the
 * only ordering there is. A metadata frame carries no sequence because it is
 * not part of that order, and a client tells the two apart by the `metadata`
 * discriminator rather than by a sequence it would have to invent.
 */
export type RendererChatFrame = RendererSequencedEvent | RendererChatMetadata;

/**
 * Chat metadata pushed to an open client, outside the turn journal.
 */
export type RendererChatMetadata = { "metadata": "titled", title: string, };

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
export type RendererToolName = "search" | "list_sources" | "read_source" | "read_tool_result" | "web_search" | "web_extract" | "read_delegated_file" | "read_file" | "list_dir" | "write_file" | "request_folder_access" | "connect_folder" | "list_connected_folders" | "list_folder" | "read_connected_file" | "import_connected_file" | "write_output_to_connected_folder" | "spawn_sandbox_agent" | "wait_for_agents" | "ask_user_questions" | "exit_plan_mode" | "exec" | "other";

export type RendererToolStatus = "completed" | "failed";

/**
 * Non-authoritative, well-known starting location for the native picker.
 *
 * This is deliberately not a free-form path. The trusted desktop decides how
 * (or whether) to map it to a local picker location.
 */
export type RequestedFolderHint = "documents" | "downloads";

/**
 * One thing a call surfaced.
 *
 * Three fields because a row reads as three: what it is, where it came from,
 * and how big or how many. A tool that wants to say more than that is
 * describing something this projection does not cover.
 */
export type ResultEntry = { kind: ResultEntryKind, 
/**
 * The row's name — a file name, a source title, a page title.
 */
label: string, 
/**
 * A secondary hint beside the name — a path, a domain, a section.
 */
detail: string | null, 
/**
 * Trailing meta — a size, a count, a status word.
 */
meta: string | null, 
/**
 * The document's media type, when the row is a document with one.
 *
 * Data rather than display text: the renderer maps it to a type-specific
 * icon (a PDF mark, a spreadsheet mark) and never prints it. Still not a
 * glyph name — the vocabulary is media types, and the renderer keeps its
 * own closed mapping with a generic fallback. `default` because retained
 * projections predate the field.
 */
media_type: string | null, };

/**
 * What one row of a listed result is, which is what picks its icon.
 *
 * A closed vocabulary rather than an icon name: the renderer chooses how to
 * draw a folder, and a tool must not be able to name a glyph.
 */
export type ResultEntryKind = "file" | "folder" | "source" | "passage" | "link" | "output";

/**
 * One thing a call could not do.
 *
 * A batch tool succeeds and fails in the same breath — five files import, two
 * do not — and a card that lists only what worked is not reporting, it is
 * flattering. Every one of Brightwave's local-file results carries a parallel
 * failures list for exactly this reason.
 *
 * Two fields because that is what a failure row reads as, and it is what
 * Brightwave's own card normalizes its three failure shapes down to before
 * rendering them: the thing that failed, and why.
 */
export type ResultFailure = { 
/**
 * What failed, when the tool can name it. `None` when the tool cannot —
 * a folder it could not even read the name of — and the row then leads
 * with a generic noun rather than being dropped. A failure the reader
 * never sees is worse than one it cannot fully name.
 */
label: string | null, 
/**
 * Why it failed, in the tool's own words.
 *
 * This is tool-authored text, and it crosses on the same terms as a
 * command's stderr already does: what the boundary keeps out is model- and
 * provider-authored text and private diagnostics, and this is a message
 * our own tool wrote for a person to act on. Clamped like every other
 * field; a failure nobody can read is not a report.
 *
 * "Our own tool wrote" is the load-bearing part, and it is a habit rather
 * than something the type can enforce. Write the sentence — "file is not
 * valid UTF-8" — instead of forwarding a `std::io::Error`, a broker
 * failure, or any other error whose `Display` you do not control. Those
 * interpolate whatever they were handed, which is how a host path ends up
 * on a card nobody meant to put it on.
 */
error: string, };

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
 * One durable "don't ask again" the reader has made, with enough provenance
 * to recognize it later and withdraw it. Grant scopes are already closed
 * renderer-safe projections, so the snapshot carries them verbatim.
 */
export type StandingGrantSnapshot = { 
/**
 * The approval decision that created the grant — also the handle a
 * revocation names.
 */
source_call_id: CallId, 
/**
 * How far the grant reaches — one chat, or every chat in a project.
 */
level: GrantLevel, 
/**
 * The name of whatever the level points at, for provenance. `None` when
 * that chat or project is untitled.
 */
level_title: string | null, action: RendererToolName, approval: ToolApprovalKind, scope: GrantScope, granted_at: string, };

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
end_published_at: string | null, } | { "tool": "web_extract", url: string, };

/**
 * Closed immutable consent semantics stored with each approval request.
 *
 * Each presentable variant names the egress a human is consenting to, so the
 * renderer can describe the action without ever seeing the model-authored
 * summary or arguments. `Unsupported` is the fail-closed default: a Sensitive
 * action the server can only reject, never approve.
 */
export type ToolApprovalKind = "search_may_share_query_and_excerpts" | "web_search_may_share_query" | "web_extract_may_fetch_url" | "exec_may_run_networked_command" | "external_mcp_may_call_server" | "workspace_may_modify_files" | "unsupported";

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
output_truncated: boolean, stdout: string, stderr: string, 
/**
 * Preview images emitted by the command, in model-facing priority order.
 */
images?: Array<ImageRef>, 
/**
 * Durable outputs the command's `output/` files created or updated.
 * Files that still match their published version are not news and are
 * not listed. Defaulted so exec rows persisted before the field
 * existed read back unchanged.
 */
outputs?: Array<ResultEntry>, } | { "tool": "web_search_provider_required" } | { "tool": "mcp_app", 
/**
 * The configured MCP server namespace that serves the view.
 */
server: string, 
/**
 * The validated `ui://` document reference.
 */
resource_uri: string, } | { "tool": "entries", entries: Array<ResultEntry>, 
/**
 * What the same call could not do. Bounded and counted on the same
 * terms as `entries`, and elided into the same tally: the card's job
 * is to be honest about how much it is not showing, and a hidden
 * failure is the worst thing to hide.
 */
failures: Array<ResultFailure>, 
/**
 * Rows past [`MAX_RESULT_ENTRIES`], counted rather than shown. A card
 * that silently lists the first fifty of two hundred results is
 * telling the reader something false.
 */
elided: number, };

/**
 * One renderer-safe source document attached to a historical user message.
 */
export type TranscriptFileAttachment = { document_id: DocumentId, name: string, media_type: string, };

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
export type TranscriptRole = "user" | "assistant" | "system";

/**
 * Why a turn failed, closed and coarse enough to be stable.
 *
 * A failure's `kind` is an internal diagnostic vocabulary: it grows with the
 * server, and its `message` can carry provider diagnostics and host paths, so
 * neither crosses to the renderer. What a client actually needs is narrower —
 * what to tell the person, and whether running the same turn again could
 * plausibly do anything different. This enum is exactly that, and nothing is
 * worth a variant here unless a client would say or do something different
 * for it.
 *
 * It is also the worker's own retry taxonomy — the same classification decides
 * whether a failed turn is rescheduled — so the category a client sees and the
 * category the scheduler acted on cannot drift apart.
 */
export type TurnFailureCategory = "rate_limited" | "auth" | "transient" | "unknown";

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
 * selection, credential presence, and the configured instance URL — key
 * material never crosses the secret boundary.
 */
export type WebSearchConfigInfo = { provider?: WebSearchProviderKind, timeout_ms: number, 
/**
 * Whether a key is stored for the selected provider. Always false for a
 * credential-free provider, which has no key slot at all — read
 * [`Self::available`] to know whether search will actually run.
 */
has_credential: boolean, 
/**
 * Whether the selected provider has everything it needs to be invoked.
 *
 * A key for the credentialed providers, an instance URL for SearXNG.
 */
available: boolean, 
/**
 * The configured SearXNG instance URL, in the canonical form the host
 * stored. It is safe to return: validation forbids embedded credentials.
 */
searxng_base_url?: string, };

/**
 * Credential readiness for one fixed web-search provider. This public shape
 * deliberately carries no secret material.
 */
export type WebSearchCredentialReadiness = { provider: WebSearchProviderKind, has_credential: boolean, };

/**
 * A configured web-search backend. The stable string also selects its secret
 * reference; it is intentionally not a model-controlled argument.
 */
export type WebSearchProviderKind = "exa" | "tavily" | "brave" | "searxng";

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
  "web_extract",
  "read_delegated_file",
  "read_file",
  "list_dir",
  "write_file",
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
  "exit_plan_mode",
  "exec",
  "other",
] as const;
