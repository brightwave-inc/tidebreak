// Generated from Rust. Do not edit.
//
// Regenerate: UPDATE_WIRE_TYPES=1 cargo test -p tidebreak-server
//
// These describe the JSON the server actually sends, derived from the same
// serde attributes serde itself reads. They are the input to the runtime
// validators in api.ts, not a replacement for them: the validators enforce
// bounds, reject control characters, and cross-check server policy, none of
// which a type can express. See docs/wire-types.md.

/**
 * A bounded headline for one background-agent activity step.
 *
 * This is deliberately smaller than [`ToolActionPreview`]. The command,
 * arguments, query, and summary are model-authored text: they are bounded for
 * safe single-line presentation, but may repeat any information the background
 * agent already saw and are therefore outside the host-field non-disclosure
 * guarantee. The projection copies no stored result text except a settled
 * `exec` receipt's bounded tail — see [`Self::with_exec_result`] — and never
 * copies host-only broker identity directly. Its only other host-derived
 * values are a typed numeric exit code and the leaf name of the canonically
 * admitted delegated file.
 *
 * Command and search fields use the foreground approval-card projection so
 * both surfaces share the same sanitization. The summary is projected the
 * same way but shown only here and on the result card, never on an approval
 * card — see `docs/decisions/0018-tool-call-narration.md`.
 */
export type AgentActivityDetail = { "kind": "exec", command: string, args: Array<string>, 
/**
 * The model's own sentence about this step, when it wrote one.
 *
 * Display-only, exactly as on [`ToolActionPreview`]: nothing in the
 * background path reads it, and no step's identity depends on it.
 */
summary?: string, exit_code?: number, output?: string, } | { "kind": "search", query: string, } | { "kind": "file", name: string, };

/**
 * One renderer-safe entry in a background run's ordered activity history.
 *
 * Built on read from durable sandbox tool calls and their immutable receipts.
 * `detail` admits bounded model-authored command/argument/query text, which may
 * repeat anything the child already saw and is not covered by the host-field
 * non-disclosure guarantee. Stored result text is copied in one place only: a
 * settled `exec` step carries its receipt's bounded tail, because that text is
 * the command's own output from a private workspace and is what makes a failed
 * step readable. Web-search and delegated-file results stay server-side. The
 * other host-derived values are the numeric exit code parsed from a receipt's
 * first line and the delegated file's leaf name. Full broker paths and root
 * identities, provider identities, executor leases, and diagnostics are never
 * copied.
 *
 * No separate activity-history shape is persisted. The optional field keeps
 * the wire additive for older clients and lets calls without derivable detail
 * retain the original `{kind, outcome, at}` shape.
 */
export type AgentActivityHistoryItem = { kind: AgentActivityKind, outcome: AgentActivityOutcome, at: string, detail?: AgentActivityDetail, };

/**
 * Fixed, renderer-safe names for supported live work.
 *
 * Adding a durable tool does not automatically expose it to a renderer: it
 * must be deliberately admitted here with a safe label.
 */
export type AgentActivityKind = "exec" | "web_search" | "update_task_plan" | "read_delegated_file" | "list_connected_folders" | "list_folder" | "read_connected_file" | "import_connected_file";

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
 * Every run executes inside the Tidebreak server process today. A run
 * executing inside an execution provider's boundary adds a variant here
 * rather than a second meaning to [`AgentRunTier`].
 */
export type AgentRunExecutionLocation = "in_process" | "container";

/**
 * Identifies one durable foreground or sandboxed background agent run.
 */
export type AgentRunId = string;

/**
 * One line of live progress a background run published, as the renderer sees
 * it.
 *
 * The text is the run's own bounded narration — the same class of prose the
 * terminal `terminal_text` already carries, published while the run is still
 * working instead of only at the end. It is model-authored and may repeat
 * information the run already saw. Stored tool records and host-owned fields
 * are not copied directly into it. Typed activity headlines are projected
 * separately.
 */
export type AgentRunProgressLine = { 
/**
 * Monotonic per-run ordering. Pass the page's `next_sequence` back as
 * `after_sequence` to read only what has arrived since.
 */
sequence: number, text: string, at: string, };

/**
 * One resumable page of a background run's live progress.
 */
export type AgentRunProgressPage = { entries: Array<AgentRunProgressLine>, 
/**
 * The cursor to resume from: the highest sequence in this page, or the
 * requested cursor when the page is empty. A reader that polls with this
 * value never re-reads a line it already has.
 */
next_sequence: number, };

/**
 * Renderer-safe state for one agent run.
 *
 * Worker lease tokens, scheduling budgets, and other executor-facing fields
 * intentionally remain inside the server/store boundary.
 */
export type AgentRunSnapshot = { id: AgentRunId, parent_id: AgentRunId | null, tier: AgentRunTier, execution_location: AgentRunExecutionLocation, 
/**
 * Active host code-execution backend for `exec`, not the run-loop seat.
 *
 * See [`ExecProviderSnapshot`]. Read from the current host
 * setting at list time — the same selection the next `exec` would use.
 */
code_execution_provider: ExecProviderSnapshot, status: AgentRunStatus, 
/**
 * Completed provider calls accumulated across every attempt.
 */
model_steps: number, 
/**
 * Disjoint provider usage accumulated across every attempt. Providers
 * without cache telemetry leave both cache fields at zero.
 */
usage: AgentRunUsageSnapshot, 
/**
 * The exact bounded task delegated by the visible spawn step.
 */
task: string | null, started_at: string | null, finished_at: string | null, 
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
 * Files a background run submitted as its deliverables, in its own order.
 *
 * A background run produces outputs by writing files and submitting them
 * by name; nothing here is host-authored, and a run that submitted nothing
 * carries an empty list.
 */
submitted_outputs: Array<SubmittedOutputSnapshot>, 
/**
 * How far this run's own task plan has got, when it keeps one.
 *
 * The full list is its own route; the snapshot carries only what a status
 * row needs — how many steps are done, and the one step being worked on.
 *
 * Omitted rather than null when there is no plan, which keeps the wire
 * additive: every run before this field existed, and every foreground
 * coordinator, reads back in the shape it always had.
 */
task_plan?: AgentRunTaskPlanProgress, 
/**
 * Bounded terminal display text returned to the parent, if settled.
 */
terminal_text: string | null, created_at: string, updated_at: string, spawn_call_id: CallId | null, };

/**
 * Durable lifecycle of an [`AgentRun`].
 */
export type AgentRunStatus = "active" | "queued" | "running" | "cancelling" | "waiting" | "retry_wait" | "needs_input" | "completed" | "failed" | "cancelled";

/**
 * Renderer-safe durable projection of one background run's current plan.
 *
 * The run-scoped twin of [`TaskPlan`]. It carries no turn: a background run
 * is one delegated task from start to finish, so the run is the only scope
 * its plan ever had.
 */
export type AgentRunTaskPlan = { run_id: AgentRunId, 
/**
 * The steps, in order.
 */
steps: Array<TaskPlanStep>, 
/**
 * When the last replacement committed.
 */
updated_at: string, };

/**
 * A run's plan as a status row needs it: the count, and the current step.
 *
 * Step text is model-authored, like the command and query headlines the
 * activity history carries. The difference is that those go through the
 * renderer clamp on the way out and this does not, so the tool boundary
 * rejects on the way in against the very same predicate: a step longer than
 * [`tidebreak_core::MAX_TASK_PLAN_STEP_CHARS`], or carrying any character the
 * preview clamp would strip — control characters, the line and paragraph
 * separators, the bidi overrides and isolates — never becomes a stored step.
 * Copying it as stored is therefore not a gap in the clamp; it is the same
 * rule enforced one surface earlier.
 */
export type AgentRunTaskPlanProgress = { completed: number, total: number, 
/**
 * The one step marked `in_progress`, when there is one.
 */
current: string | null, updated_at: string, };

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
 * Renderer-safe disjoint token accounting for one background run.
 */
export type AgentRunUsageSnapshot = { input_tokens: number, output_tokens: number, cache_read_input_tokens: number, cache_creation_input_tokens: number, };

/**
 * One question as it was asked, with what the reader chose.
 *
 * Labels rather than option ids: the recap is read by a person, and the ids
 * are an internal handle they never saw. A question the reader skipped
 * carries neither a selection nor an answer, which is how the card knows to
 * say so.
 */
export type AnsweredUserQuestion = { 
/**
 * The prompt the reader answered.
 */
question: string, 
/**
 * Labels of the options chosen, in the order the question listed them.
 */
selected?: Array<string>, 
/**
 * The reader's own words, when the question allowed them.
 */
custom_answer?: string, };

/**
 * One app's detail: the summary fields plus its revision history.
 */
export type AppDetail = { id: AppId, name: string, 
/**
 * Creation time of the first revision.
 */
created_at: string, 
/**
 * Creation time of the current revision.
 */
updated_at: string, 
/**
 * Revision currently presented as the app's content.
 */
current_revision: AppRevisionId, 
/**
 * Every retained revision, newest first.
 */
revisions: Array<AppRevisionSummary>, };

/**
 * What asking for an app's gateway page came back as.
 *
 * A closed union rather than prose, because the renderer branches on it: only
 * `ready` carries somewhere to go, and only `refused` and `unreachable` carry
 * words worth showing verbatim — a gateway that will not hold a bundle names
 * what about it, and no wording assembled here could.
 */
export type AppGatewayPageOutcome = "ready" | "no_gateway" | "not_registered" | "refused" | "unreachable";

/**
 * One answer: an outcome, and whatever came with it.
 */
export type AppGatewayPageResult = { outcome: AppGatewayPageOutcome, 
/**
 * The app's page at the gateway, present exactly when `outcome` is
 * `ready`.
 */
url?: string, 
/**
 * The gateway's own message, when the outcome carries one.
 */
message?: string, };

/**
 * One current-manifest binding, projected for the consent sheet.
 *
 * Exactly one of `app`, `folder`, and `gateway_app` is present, matching what
 * the binding names: a local record id, a broker root id, or the gateway's
 * own connected-app id. An app-keyed or gateway row carries `operation_ids`;
 * a folder row carries `access`. A gateway row names the gateway's app and
 * the operations pinned under it, and — once the live read answers — that
 * app's display name; the gateway's deployment URL is never projected, the
 * same names-only posture the `rest_api` rows hold. The sheet derives the
 * combined-consent exfiltration warning (docs/folder-bindings.md) from the
 * rows themselves: a manifest with both a folder row and a network row —
 * local or gateway — can read files and reach the network.
 */
export type AppGrantBindingState = { 
/**
 * Connected app the manifest binds, by record id, for an app-keyed
 * binding.
 */
app: ConnectedAppId | null, 
/**
 * Connected folder the manifest binds, by broker root id, for a folder
 * binding.
 */
folder: HostRootId | null, 
/**
 * Gateway connected app the manifest binds, by the gateway's own app id,
 * for a gateway binding. The id alone — never the gateway that serves
 * it.
 */
gateway_app: string | null, 
/**
 * The access level a folder binding requests.
 */
access: FolderAccess | null, 
/**
 * The bound connected app's, folder's, or gateway app's display name,
 * absent when nothing configured, approved, or readable answers to the
 * id — the sheet says so instead of showing a raw id alone.
 */
name: string | null, 
/**
 * Catalog `operationId`s the current manifest pins under this app, for a
 * `rest_api` or gateway binding.
 */
operation_ids: Array<string> | null, 
/**
 * Whether the live grant covers every listed capability under this
 * binding and its target still matches the granted fingerprint.
 */
granted: boolean, 
/**
 * Whether a grant names this binding's target but it changed — a
 * reconfigured record, a disconnected folder — since consent: the
 * "changed since you agreed" affordance, distinct from a binding that
 * was simply never granted.
 */
definition_changed: boolean, };

/**
 * Renderer-safe grant state for one app: the consent sheet's whole input.
 *
 * `bindings` follows the app's **current** revision's manifest — ids and
 * names only. The definitions behind the connected apps, the paths behind
 * the folders, and any environment or credential values they select, are
 * deliberately absent from this projection.
 */
export type AppGrantState = { 
/**
 * Whether a live grant fully covers the current manifest with every
 * bound definition unchanged since consent — the "no sheet needed"
 * verdict. A manifest with no bindings is vacuously granted: there is
 * nothing to consent to, so no sheet is shown. When `false`,
 * (re-)consent is required before every pinned capability is invokable.
 */
granted: boolean, 
/**
 * The current manifest's bindings, one entry per bound connected app or
 * folder.
 */
bindings: Array<AppGrantBindingState>, };

/**
 * Identifies one profile-scoped local app across all of its revisions.
 *
 * Like [`OutputId`] this is a durable opaque handle: possession is not
 * authority, and it never encodes a name or a host path. Unlike an
 * output, an app has no owning conversation — the profile owns it.
 */
export type AppId = string;

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
export type AppInvokeRefusalKind = "app_not_found" | "not_pinned" | "consent_required" | "unknown_tool" | "gateway_unavailable" | "gateway_authorization_required";

/**
 * The library listing: every live app, newest activity first.
 */
export type AppLibrary = { apps: Array<AppSummary>, };

/**
 * Identifies one immutable revision of a local app.
 */
export type AppRevisionId = string;

/**
 * One revision row: identity and position only. The manifest and the
 * bundle's content address stay server-side.
 */
export type AppRevisionSummary = { id: AppRevisionId, 
/**
 * One-based position in the app's revision history.
 */
ordinal: number, created_at: string, };

/**
 * One library row.
 */
export type AppSummary = { id: AppId, 
/**
 * Display name, following the current revision's manifest.
 */
name: string, 
/**
 * Number of retained revisions, always at least one.
 */
revision_count: number, 
/**
 * Creation time of the current revision.
 */
updated_at: string, 
/**
 * Whether a live grant fully covers the app right now — the same
 * verdict `GET /apps/{id}/grant` reports, so the library badge and the
 * consent sheet can never disagree.
 */
granted: boolean, };

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
 * Outcome recorded on [`CodeEvent::ApprovalResolved`].
 */
export type ApprovalDecisionKind = { "type": "approve" } | { "type": "deny", 
/**
 * Feedback returned to the engine, when any.
 */
feedback?: string, } | { "type": "abandoned" };

/**
 * How wide a standing grant the human chose, narrowest first.
 *
 * The renderer names a rung; the server builds the concrete grant from the
 * arguments the call is parked on. A grant can therefore only ever describe
 * the action that was actually under review.
 */
export type ApprovalGrantRung = "exact_action" | { "command_prefix": { tokens: number, } } | { "path_prefix": { segments: number, } } | "whole_tool";

/**
 * Opaque renderer identity for one assistant-message citation.
 */
export type AssistantCitationId = string;

/**
 * Renderer-safe historical citation stored beside an assistant message.
 */
export type AssistantCitationSnapshot = { id: AssistantCitationId, ordinal: number, document_id: DocumentId, locator: CitationLocator, };

/**
 * An attention state together with the source that produced it.
 *
 * [`AttentionState::NeedsYou`] also carries a source so a structured need
 * stays distinguishable after the pair is stored as JSON. The two sources
 * must agree when the state is `NeedsYou`; [`Attention::needs_you`] enforces
 * that at construction.
 */
export type Attention = { 
/**
 * The state.
 */
state: AttentionState, 
/**
 * Who or what set it.
 */
source: AttentionSource, };

/**
 * Why the current attention state was chosen.
 */
export type AttentionSource = "structured" | "heuristic" | "lifecycle" | "user";

/**
 * Server-computed attention for one unit of supervised work.
 */
export type AttentionState = { "type": "working" } | { "type": "needs_you", 
/**
 * Short prompt shown on the badge.
 */
prompt: string, 
/**
 * How this need was detected.
 */
source: AttentionSource, } | { "type": "stalled", 
/**
 * Seconds of observed silence.
 */
idle_secs: number, } | { "type": "done_unreviewed" } | { "type": "idle" } | { "type": "fenced", 
/**
 * Why it was fenced.
 */
reason: FenceReason, } | { "type": "manual", 
/**
 * User-supplied note.
 */
note: string, };

/**
 * Where the Auto-mode judge stands on one parked call.
 *
 * The marker is load-bearing for the renderer: without it, "the judge is
 * still deciding" and "the judge declined, a human is needed" are both just
 * `Pending`, indistinguishable except by waiting.
 */
export type AutoJudgeStatus = "judging" | "approved" | "declined";

/**
 * Bounded error carried on [`CodeEvent::TurnFailed`].
 */
export type BoundedError = { 
/**
 * Short message, already truncated by the adapter.
 */
message: string, };

/**
 * Identifies one tool call, stable across its request/approval/result.
 */
export type CallId = string;

/**
 * Whether an adapter can honor a capability for a probed engine version.
 */
export type CapLevel = "supported" | "unsupported" | "unknown";

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
 * Outbound network access for code execution in this chat.
 */
network_policy: NetworkPolicy, 
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

export type ChatGptSignInStatus = { signed_in: boolean, pending_authorization_url?: string, error?: string, };

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
file_attachments?: Array<TranscriptFileAttachment>, 
/**
 * Skills this user message explicitly invoked, in submitted order. Absent
 * for the ordinary message that invoked none.
 */
invoked_skills?: Array<string>, };

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
 * A completed turn points at its authoritative assistant output. A cancelled
 * turn may point at the last assistant message it committed before stopping;
 * message-less failed and cancelled turns remain first-class transcript entries
 * carrying the partial prose and reasoning the reader already saw live.
 */
export type ChatTerminalTurnSnapshot = { turn_id: TurnId, message_id?: MessageId, status: ChatTerminalTurnStatus, partial_content: string, reasoning?: string, refusal?: RendererRefusal, failure_category?: TurnFailureCategory, failure_detail?: string, failure_model?: RendererModelIdentity, file_changes: Array<ExecFileChangeSummary>, 
/**
 * Skills the user explicitly invoked for this turn, in submitted order.
 * Absent for the ordinary turn that invoked none.
 */
invoked_skills?: Array<string>, 
/**
 * Token accounting for the turn, so a freshly opened chat can show
 * context usage without waiting for the next turn to finish.
 */
usage: RendererTurnUsage, voice_input_used: boolean, finished_at: string, };

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
export type ChatToolActivityStatus = "completed" | "failed" | "denied" | "cancelled";

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
 * Hint that a turn recorded a checkpoint. The diff body is loaded separately.
 */
export type CheckpointHint = { 
/**
 * Hidden ref name, when known.
 */
checkpoint_ref?: string, 
/**
 * Bounded diffstat.
 */
diffstat?: Diffstat, };

/**
 * A small, human-scale position inside a cited document.
 *
 * Validation is intentionally loose. A page or line that does not exist still
 * renders and opens the document as close to that position as the reader can.
 */
export type CitationLocator = { "kind": "document" } | { "kind": "page", page: number, } | { "kind": "pages", start: number, end: number, } | { "kind": "lines", start: number, end: number, } | { "kind": "sheet", sheet: string, cells: string | null, };

/**
 * Bounded output of one named quick action. Never journaled.
 */
export type CodeActionSnapshot = { name: string, success: boolean, exit_code?: number, stdout: string, stderr: string, timed_out: boolean, };

/**
 * One UTC day in an analytics trend.
 */
export type CodeAnalyticsDay = { date: string, sessions: number, turns: number, total_tokens: number, estimated_cost_microusd: number, pull_requests_opened: number, pull_requests_merged: number, };

/**
 * Metrics attributed to one code harness.
 */
export type CodeAnalyticsHarness = { harness_kind: HarnessKind, sessions: number, turns: number, total_tokens: number, estimated_cost_microusd: number, };

/**
 * Metrics attributed to one model and service tier.
 */
export type CodeAnalyticsModel = { model_id?: string, harness_kind: HarnessKind, fast_mode: boolean, sessions: number, turns: number, total_tokens: number, estimated_cost_microusd: number, priced: boolean, };

/**
 * How much of the report has a known local price.
 */
export type CodeAnalyticsPricingCoverage = { priced_turns: number, unpriced_turns: number, priced_tokens: number, unpriced_tokens: number, prices_as_of: string, };

/**
 * Time window for the code analytics report.
 */
export type CodeAnalyticsRange = "7d" | "30d" | "90d" | "all";

/**
 * Metrics attributed to one registered repository.
 */
export type CodeAnalyticsRepository = { repo_id: RepoId, name: string, sessions: number, turns: number, total_tokens: number, estimated_cost_microusd: number, pull_requests_opened: number, pull_requests_merged: number, };

/**
 * Owner-scoped code activity and local cost estimates.
 */
export type CodeAnalyticsSnapshot = { range: CodeAnalyticsRange, from?: string, through: string, repo_id?: RepoId, totals: CodeAnalyticsTotals, daily: Array<CodeAnalyticsDay>, repositories: Array<CodeAnalyticsRepository>, models: Array<CodeAnalyticsModel>, harnesses: Array<CodeAnalyticsHarness>, pricing: CodeAnalyticsPricingCoverage, };

/**
 * Totals for one analytics window.
 */
export type CodeAnalyticsTotals = { sessions: number, turns: number, completed_turns: number, failed_turns: number, interrupted_turns: number, running_turns: number, input_tokens: number, output_tokens: number, cache_read_tokens: number, cache_write_tokens: number, total_tokens: number, estimated_cost_microusd: number, pull_requests_opened: number, pull_requests_merged: number, };

/**
 * `approve` or `deny`.
 */
export type CodeApprovalDecision = "approve" | "deny";

/**
 * Body of `POST /code/approvals/{id}/decision`.
 */
export type CodeApprovalDecisionBody = { decision: CodeApprovalDecision, feedback?: string, };

/**
 * Identifies one parked approval belonging to a code session.
 */
export type CodeApprovalId = string;

/**
 * Best-effort classification of what an approval is asking.
 */
export type CodeApprovalKind = { "type": "command", 
/**
 * Command string.
 */
cmd: string, 
/**
 * Working directory, when reported.
 */
cwd?: string | null, } | { "type": "file_write", 
/**
 * Paths involved.
 */
paths: Array<string>, } | { "type": "network", 
/**
 * Host or summary the engine reported.
 */
summary: string, } | { "type": "other", 
/**
 * Engine-provided summary.
 */
summary: string, };

/**
 * One parked or decided engine approval.
 */
export type CodeApprovalSnapshot = { id: CodeApprovalId, session_id: CodeSessionId, turn_id: CodeTurnId, kind: CodeApprovalKind, 
/**
 * Exact JSON the engine sent, already size-capped. The card renders this.
 */
harness_raw_json: string, state: CodeApprovalState, feedback?: string, requested_at: string, decided_at?: string, };

/**
 * State of a persisted approval.
 */
export type CodeApprovalState = "pending" | "approved" | "denied" | "abandoned";

/**
 * One failing check's downloaded job log:
 * `POST /code/workspaces/{id}/pr/check-logs`.
 *
 * `path` is absolute and sits outside the Git worktree, in the same private
 * storage a fork transcript uses. The prompt names it and the engine opens
 * it; nothing is uploaded and nothing is indexable.
 */
export type CodeCheckLog = { 
/**
 * Check name as the host reports it.
 */
check: string, path: string, byte_len: number, 
/**
 * True when the file holds only the tail of the job log.
 */
truncated: boolean, 
/**
 * The job's host URL. A check without one has no log to download, so
 * every entry here has one.
 */
url: string, };

/**
 * One failing check whose log could not be read.
 */
export type CodeCheckLogError = { check: string, message: string, };

/**
 * Failing job logs written for one workspace's pull request.
 *
 * A check with no downloadable log — an external CI provider, or a check-run
 * URL that names no Actions job — is simply absent from both lists. The
 * caller still names it in the prompt from the digest it already holds.
 */
export type CodeCheckLogsSnapshot = { 
/**
 * Head the logs were read against, when the host reported one.
 */
head_sha?: string, logs: Array<CodeCheckLog>, errors: Array<CodeCheckLogError>, };

/**
 * Remembered clone destination plus observed `gh` status.
 */
export type CodeCloneDefaults = { parent_dir?: string, gh_found: boolean, gh_authenticated?: boolean, gh_remediation: string, };

/**
 * Snapshot of an in-flight or finished clone job.
 */
export type CodeCloneJobSnapshot = { id: string, phase: string, percent?: number, done: boolean, error?: string, repo_id?: RepoId, };

/**
 * Result of staging and committing the workspace worktree.
 */
export type CodeCommitSnapshot = { sha: string, message: string, stat: Diffstat, };

/**
 * Delivery mutation result. A partial rerun returns every per-run outcome.
 */
export type CodeDeliveryActionResult = { success: boolean, message: string, rerun_outcomes?: Array<CodeDeliveryRerunOutcome>, };

/**
 * One CI check, enriched with the workflow run that can be rerun when known.
 */
export type CodeDeliveryCheck = { name: string, bucket: PullRequestCheckBucket, detail?: string, url?: string, workflow_run_id?: number, };

export type CodeDeliveryDeploymentStatus = { id: number, state: string, description: string, environment_url?: string, log_url?: string, created_at: string, };

/**
 * Why an open pull request belongs in the default Needs attention view.
 */
export type CodeDeliveryPrAttentionReason = "changes_requested" | "checks_failed" | "conflicts" | "behind" | "blocked";

/**
 * User-initiated global PR action. Code-changing actions deliberately do not
 * exist here; they remain workspace-scoped agent prompts.
 */
export type CodeDeliveryPullRequestAction = { "type": "mark_ready" } | { "type": "merge", method: CodePrMergeMethod, auto: boolean, admin: boolean, expected_head_sha: string, } | { "type": "create_stack", 
/**
 * The chain to register, bottom to top. Every pull request's base
 * ref must match the previous one's head ref.
 */
numbers: Array<number>, } | { "type": "rerun_failed", workflow_run_ids: Array<number>, } | { "type": "close" } | { "type": "reopen" } | { "type": "comment", body: string, };

export type CodeDeliveryPullRequestActionBody = { target: CodeDeliveryPullRequestTarget, action: CodeDeliveryPullRequestAction, };

/**
 * Full PR drawer payload. Conversation entries retain the existing bounded
 * comment contract used by workspace PRs.
 */
export type CodeDeliveryPullRequestDetail = { summary: CodeDeliveryPullRequestSummary, body: string, labels: Array<string>, assignees: Array<string>, requested_reviewers: Array<string>, changed_files: number, additions: number, deletions: number, 
/**
 * The full stack chain this pull request belongs to, bottom to top,
 * when the host reported one. Absent on hosts without stacked pull
 * requests.
 */
stack?: Array<CodeDeliveryStackMember>, commits: number, merged_by?: string, 
/**
 * Empty when the diff could not be read. Truncated by `files_truncated`
 * rather than paged: the panel is a review aid, not a diff viewer.
 */
files: Array<CodeDeliveryPullRequestFile>, files_truncated: boolean, comments: Array<PullRequestComment>, 
/**
 * Section reads that failed after the pull request itself loaded.
 */
errors: Array<CodeDeliverySourceError>, can_mark_ready: boolean, can_merge: boolean, can_rerun_failed: boolean, can_close: boolean, can_reopen: boolean, can_comment: boolean, };

/**
 * One file in a pull request's diff.
 *
 * `patch` is the host's unified hunk text and is absent for binary files and
 * for diffs GitHub declines to render. It is bounded by the host, not stored.
 */
export type CodeDeliveryPullRequestFile = { path: string, 
/**
 * `added`, `modified`, `removed`, `renamed`, `copied`, or `changed`.
 */
status: string, additions: number, deletions: number, previous_path?: string, patch?: string, };

/**
 * Server-side PR query. Saved views are client-owned; their resolved filters
 * are sent here so paging remains bounded across many repositories.
 */
export type CodeDeliveryPullRequestQuery = { repositories: Array<CodeGitHubRepositoryTarget>, search?: string, states: Array<string>, review_states: Array<string>, check_states: Array<string>, authors: Array<string>, attention_only: boolean, ready_only: boolean, tidebreak_linked?: boolean, updated_after?: string, cursor?: string, limit?: number, 
/**
 * Skip the short list cache and reread GitHub.
 *
 * Set only by an explicit user refresh. Paging never sets it, so
 * following a cursor stays on the aggregate the first page came from.
 */
refresh: boolean, };

/**
 * Pull request row shared by the overview and notification monitor.
 */
export type CodeDeliveryPullRequestSummary = { id: string, repository: CodeGitHubRepositoryRef, number: number, url: string, title: string, state: string, draft: boolean, author?: string, author_avatar_url?: string, head_branch: string, base_branch: string, head_sha?: string, review_decision?: string, mergeable?: string, merge_state_status?: string, auto_merge_enabled: boolean, 
/**
 * True when the last reliable host observation placed the pull request
 * in its merge queue. Absent when the list read cannot answer.
 */
in_merge_queue?: boolean, 
/**
 * Issue comments visible from the list read. Review and inline comments
 * remain detail-only, so an absent count means unknown rather than zero.
 */
comment_count?: number, checks: Array<CodeDeliveryCheck>, attention_reasons: Array<CodeDeliveryPrAttentionReason>, ready_to_merge: boolean, workspace_links: Array<CodeDeliveryWorkspaceLink>, 
/**
 * The host stack this pull request belongs to (GitHub stacked pull
 * requests), when the host reported one. Identifies the stack, not the
 * PR.
 */
stack_number?: number, 
/**
 * Total layers in that stack, bottom to top, including merged ones.
 */
stack_size?: number, 
/**
 * The pull request this one is stacked on. Host stack order wins when
 * the host reported a stack; branch inference from the durable fact set
 * is the fallback (decision 77), so a parent outside the current page
 * or filter still resolves. Absent when the base is the default branch
 * or nothing tracked owns it.
 */
stack_parent_number?: number, 
/**
 * A stack-shaped chain of inferred edges the host has no stack for,
 * bottom to top, when one resolves gaplessly around this pull request
 * and no member is host-registered. Creating the stack on GitHub makes
 * the host own the ordering, the retargeting, and the whole-chain merge
 * — without it, merging a layer lands it into the branch below rather
 * than the default branch, which is easy to do by accident.
 */
unregistered_stack_numbers?: Array<number>, labels: Array<string>, created_at: string, updated_at: string, 
/**
 * Set only once the pull request merged. `state` alone cannot separate a
 * merged pull request from a closed one on every host response, and the
 * row says *when* it settled rather than when it was last touched.
 */
merged_at?: string, closed_at?: string, };

/**
 * Target for a pull-request detail read or action.
 */
export type CodeDeliveryPullRequestTarget = { repository: CodeGitHubRepositoryTarget, number: number, };

/**
 * One page of pull requests, with repository-local failures kept alongside
 * the usable rows instead of failing the entire cross-repository query.
 */
export type CodeDeliveryPullRequestsPage = { capability: CodeGitHubCapability, items: Array<CodeDeliveryPullRequestSummary>, next_cursor?: string, errors: Array<CodeDeliverySourceError>, fetched_at: string, };

/**
 * Registered repositories that resolve to GitHub, plus partial failures.
 */
export type CodeDeliveryRepositoriesSnapshot = { capability: CodeGitHubCapability, repositories: Array<CodeGitHubRepositoryRef>, errors: Array<CodeDeliverySourceError>, fetched_at: string, };

/**
 * Result of rerunning one GitHub Actions workflow run.
 */
export type CodeDeliveryRerunOutcome = { workflow_run_id: number, success: boolean, error?: string, };

export type CodeDeliveryRunAction = { "type": "rerun" } | { "type": "rerun_failed" };

export type CodeDeliveryRunActionBody = { target: CodeDeliveryRunTarget, action: CodeDeliveryRunAction, };

export type CodeDeliveryRunAttentionReason = "failure" | "timed_out" | "action_required" | "startup_failure";

export type CodeDeliveryRunDetail = { summary: CodeDeliveryRunSummary, jobs: Array<CodeDeliveryWorkflowJob>, deployment_statuses: Array<CodeDeliveryDeploymentStatus>, can_rerun_failed: boolean, 
/**
 * Section reads that failed after the run or deployment itself loaded.
 */
errors: Array<CodeDeliverySourceError>, };

export type CodeDeliveryRunKind = "workflow_run" | "deployment";

export type CodeDeliveryRunQuery = { repositories: Array<CodeGitHubRepositoryTarget>, search?: string, kinds: Array<CodeDeliveryRunKind>, statuses: Array<string>, conclusions: Array<string>, workflows: Array<string>, environments: Array<string>, branches: Array<string>, events: Array<string>, actors: Array<string>, attention_only: boolean, tidebreak_linked?: boolean, created_after?: string, cursor?: string, limit?: number, 
/**
 * Skip the short list cache and reread GitHub. See the pull-request query.
 */
refresh: boolean, };

/**
 * Normalized Actions workflow run or GitHub deployment row.
 */
export type CodeDeliveryRunSummary = { id: string, repository: CodeGitHubRepositoryRef, kind: CodeDeliveryRunKind, github_id: number, run_attempt?: number, name: string, url: string, status: string, conclusion?: string, workflow?: string, environment?: string, branch?: string, sha?: string, event?: string, actor?: string, attention_reasons: Array<CodeDeliveryRunAttentionReason>, workspace_links: Array<CodeDeliveryWorkspaceLink>, created_at: string, updated_at: string, };

export type CodeDeliveryRunTarget = { repository: CodeGitHubRepositoryTarget, kind: CodeDeliveryRunKind, id: number, };

export type CodeDeliveryRunsPage = { capability: CodeGitHubCapability, items: Array<CodeDeliveryRunSummary>, next_cursor?: string, errors: Array<CodeDeliverySourceError>, fetched_at: string, };

/**
 * One repository-level failure in an otherwise usable aggregate response.
 */
export type CodeDeliverySourceError = { repository?: CodeGitHubRepositoryTarget, kind: string, message: string, retry_at?: string, };

/**
 * One layer of a pull-request stack, in bottom-to-top order.
 */
export type CodeDeliveryStackMember = { number: number, 
/**
 * Host state token (open, closed).
 */
state: string, draft: boolean, 
/**
 * Set once this layer merged.
 */
merged_at?: string, 
/**
 * Head branch name.
 */
head_branch: string, head_sha?: string, };

export type CodeDeliveryWorkflowJob = { id: number, name: string, status: string, conclusion?: string, url: string, started_at: string | null, completed_at: string | null, failed_steps: Array<string>, };

/**
 * One Tidebreak workspace that plausibly produced a remote delivery item.
 */
export type CodeDeliveryWorkspaceLink = { workspace_id: WorkspaceId, repo_id: RepoId, title: string, branch_name: string, status: CodeWorkspaceStatus, exact: boolean, 
/**
 * Durable attribution behind this link, when one is stored: the
 * workspace authored or contributed to the pull request (decision 77).
 * Absent on links the live heuristic derived.
 */
relation?: CodePullRequestRelation, };

/**
 * One event in an external agent-engine session's journal.
 *
 * Serialized as an internally-tagged union (a `type` field selects the
 * variant), and `#[non_exhaustive]` so new event kinds can be added without
 * breaking downstream consumers.
 */
export type CodeEvent = { "type": "session_started", 
/**
 * Which engine.
 */
harness_kind: HarnessKind, 
/**
 * Version observed at launch.
 */
harness_version: string, 
/**
 * Engine-native resume token, when the stream reported one.
 */
resume_ref?: string, } | { "type": "turn_started", 
/**
 * The turn being processed.
 */
turn_id: CodeTurnId, } | { "type": "assistant_delta", 
/**
 * The text fragment to append.
 */
text: string, } | { "type": "assistant_message", 
/**
 * The message text.
 */
text: string, 
/**
 * The `Task` call this message ran inside, when a harness subagent
 * produced it (decision 52). Absent on the parent's own messages.
 */
parent_call_id?: string, } | { "type": "reasoning_delta", 
/**
 * The reasoning fragment.
 */
text: string, } | { "type": "tool_started", 
/**
 * Engine-native call id.
 */
call_id: string, 
/**
 * Tool name as the engine reported it.
 */
name: string, 
/**
 * Display-oriented classification.
 */
detail: ToolDetail, 
/**
 * The `Task` call this call ran inside, when a harness subagent
 * issued it (decision 52). Absent on the parent's own calls.
 */
parent_call_id?: string, } | { "type": "tool_completed", 
/**
 * Engine-native call id.
 */
call_id: string, 
/**
 * How it finished.
 */
outcome: ToolOutcome, 
/**
 * Bounded preview of the result.
 */
preview: string, 
/**
 * Classification rebuilt from the call's complete arguments.
 *
 * Engines open a tool call before its arguments finish streaming, so
 * the detail on [`CodeEvent::ToolStarted`] can name nothing. This is
 * the correction: adapters that see the final arguments fill it in,
 * and renderers merge it into the started call. It is `None` when
 * the engine's completion payload carries no arguments.
 */
detail?: ToolDetail, 
/**
 * The `Task` call this call ran inside, when a harness subagent
 * issued it (decision 52). Absent on the parent's own calls.
 */
parent_call_id?: string, } | { "type": "file_changed", 
/**
 * Path relative to the worktree, when the engine reports one.
 */
path: string, 
/**
 * Kind of change.
 */
kind: FileChangeKind, 
/**
 * Bounded diffstat.
 */
diffstat: Diffstat, } | { "type": "approval_requested", 
/**
 * Hint id; the row is the source of truth.
 */
approval_id: CodeApprovalId, } | { "type": "approval_resolved", 
/**
 * The approval that was decided.
 */
approval_id: CodeApprovalId, 
/**
 * The decision.
 */
decision: ApprovalDecisionKind, } | { "type": "user_steered", 
/**
 * The steered user text, already bounded.
 */
text: string, } | { "type": "turn_completed", 
/**
 * Token accounting as reported by the engine.
 */
usage: CodeUsage, 
/**
 * Checkpoint recorded at turn end, when any.
 */
checkpoint?: CheckpointHint, } | { "type": "turn_failed", 
/**
 * Bounded error.
 */
error: BoundedError, } | { "type": "turn_interrupted" } | { "type": "checkpoint_recorded", 
/**
 * The turn that ended at this checkpoint.
 */
turn_id: CodeTurnId, 
/**
 * Bounded diffstat.
 */
diffstat: Diffstat, } | { "type": "harness_notice", 
/**
 * Severity.
 */
level: HarnessNoticeLevel, 
/**
 * Bounded message.
 */
message: string, } | { "type": "attention_changed", 
/**
 * New state.
 */
state: AttentionState, 
/**
 * Who or what set it.
 */
source: AttentionSource, };

/**
 * One changed path in a workspace or turn file list.
 */
export type CodeFileChange = { path: string, kind: FileChangeKind, insertions: number, deletions: number, previous_path?: string, };

/**
 * Body of `POST /code/sessions/{id}/fork`. An absent body forks at the
 * newest turn.
 */
export type CodeForkBody = { 
/**
 * Fork at the end of this turn; later turns stay out of the handoff.
 */
at_turn?: CodeTurnId, };

/**
 * A written fork handoff: `POST /code/sessions/{id}/fork`.
 *
 * `path` is the condensed transcript, absolute under private storage so a
 * child agent of any engine can read it without Git ever indexing it. `dir`
 * is the fork's own directory, which also holds one full per-turn record —
 * `turn-0007.md` for turn 7 — and any retained image attachments.
 */
export type CodeForkTranscript = { path: string, dir: string, byte_len: number, 
/**
 * Complete turn histories the condensed transcript renders in full.
 */
turns: number, 
/**
 * Turns the fork covers, up to and including the fork point.
 */
total_turns: number, 
/**
 * The fork point's turn ordinal, present when the conversation
 * continued past it — later turns are excluded from the handoff.
 */
at_turn_ordinal?: number, 
/**
 * True when bounded replay omitted whole turn histories or the file size
 * cap reduced the oldest turns or the end of one oversized turn.
 */
truncated: boolean, };

/**
 * Whether this caller's GitHub path can serve Delivery requests.
 *
 * Desktops and self-host machines report the local GitHub CLI. A
 * gateway-authenticated hosted machine reports the caller's connected forge.
 */
export type CodeGitHubCapability = { found: boolean, authenticated?: boolean, viewer_login?: string, remediation: string, };

/**
 * GitHub repository identity used by the install-wide delivery surfaces.
 *
 * `host` keeps GitHub Enterprise repositories distinct without introducing a
 * generic provider abstraction. `tidebreak_repo_id` is present only when the
 * repository was resolved from the current owner's registered local catalog.
 */
export type CodeGitHubRepositoryRef = { host: string, owner: string, name: string, name_with_owner: string, url: string, default_branch?: string, tidebreak_repo_id?: RepoId, };

/**
 * Minimal repository selector accepted by delivery query and action routes.
 */
export type CodeGitHubRepositoryTarget = { host: string, owner: string, name: string, };

/**
 * `GET /code/repos/github`: repositories this caller can clone.
 */
export type CodeGithubRepositories = { repositories: Array<CodeGithubRepository>, };

/**
 * One GitHub repository the add-repository picker can offer.
 */
export type CodeGithubRepository = { full_name: string, private: boolean, description?: string, };

/**
 * State of one warm harness install, returned by
 * `POST /code/harnesses/{kind}/install` and restated on the live bus.
 *
 * `phase` is `installing`, `ready`, or `failed`. npm reports no usable
 * percentage to a pipe, so there is no bar to show — only which of the three
 * the engine is in.
 */
export type CodeHarnessInstallSnapshot = { kind: HarnessKind, 
/**
 * The pinned version being installed.
 */
version?: string, phase: string, done: boolean, error?: string, };

/**
 * `GET /code/workspaces/{id}/pr/comments`: the PR conversation, read live
 * from the host and never persisted.
 */
export type CodePrCommentsSnapshot = { 
/**
 * PR number the comments belong to.
 */
number: number, 
/**
 * Issue comments, review bodies, and inline review comments, ordered by
 * creation time.
 */
comments: Array<PullRequestComment>, };

/**
 * Merge strategy for a user-initiated PR merge.
 */
export type CodePrMergeMethod = "squash" | "merge" | "rebase";

/**
 * How strongly a workspace is tied to a pull request (decision 77).
 *
 * Only two acts mint attribution: `gh pr create` (authored) and a push whose
 * branch is or becomes a pull request's head (contributed). Reading,
 * checking out, commenting on, closing, or merging a pull request never
 * does, so review and triage agents stay out of the attributed set.
 */
export type CodePullRequestRelation = "authored" | "contributed";

/**
 * Result of pushing the workspace branch.
 */
export type CodePushSnapshot = { branch: string, remote: string, };

/**
 * A registered local git repository.
 */
export type CodeRepoSnapshot = { id: RepoId, root_path: string, display_name: string, default_base_ref: string, branch_prefix: string, setup_script?: string, archive_script?: string, quick_actions: Array<QuickAction>, created_at: string, };

/**
 * One way of adding a repository, and whether this machine can serve it.
 *
 * `kind` is `local`, `git_url`, or `github`. `remediation` says what stands in
 * the way, and rides on an available source too: `github` clones anything
 * public without a `gh` credential, so its absence is a note about private
 * repositories rather than a reason to withhold the form.
 */
export type CodeRepoSource = { kind: string, available: boolean, remediation?: string, };

/**
 * What this machine can add a repository from: `GET /code/repos/sources`.
 *
 * The machine answers for itself — whether it can spawn `git`, whether it has
 * a GitHub credential — and the client decides separately whether it can
 * offer a picker for any of it. Those are different questions: a desktop on
 * the same computer as its machine can browse for a path, and a window
 * attached to a machine elsewhere cannot, while the machine's own answer is
 * identical either way.
 *
 * `chooses_destination` says the machine places clones itself — a stored
 * destination, or the self-host default — so a caller names no path.
 *
 * Unknown source kinds are ignored by clients rather than rendered, so this
 * set may grow without a client release (decision 17).
 */
export type CodeRepoSources = { sources: Array<CodeRepoSource>, chooses_destination: boolean, };

/**
 * One repository on the reclaim surface.
 */
export type CodeRepoStorageSnapshot = { id: RepoId, display_name: string, clone_bytes: number, clone_reclaimable: boolean, workspaces: Array<CodeWorkspaceStorageSnapshot>, };

/**
 * What a running interactive session is actually occupied with. This is
 * intentionally coarser than a transcript tool name: list surfaces need to
 * distinguish agent generation, a shell, a passive monitor, and delegated
 * work without leaking command text into every digest.
 */
export type CodeSessionActivity = "agent" | "shell" | "monitor" | "subagents" | "file" | "search" | "tool";

/**
 * Cheap per-session digest on `/code/updates`.
 */
export type CodeSessionDigest = { workspace: WorkspaceId, session: CodeSessionId, kind: CodeSessionKind, 
/**
 * Engine identity for list surfaces that collapse several sessions into
 * one workspace row. Optional on the wire so a desktop can still read a
 * digest from an older server during an update.
 */
harness_kind?: HarnessKind, lifecycle: CodeSessionLifecycle, attention: Attention, title: string, turn_count: number, 
/**
 * What the live turn is occupied with, while running.
 */
activity?: CodeSessionActivity, pr_state?: PullRequestDigest, 
/**
 * How many pull requests hold a durable attribution to this workspace
 * (decision 77). Absent when none do.
 */
pr_count?: number, 
/**
 * Watch progress, present only on `kind: watch` digests.
 */
watch_state?: CodeWatchState, watch_detail?: string, watch_cycles?: number, 
/**
 * Harness subagents on this session, present only when any were
 * observed (decision 52).
 */
subagents?: Array<CodeSubagentSummary>, 
/**
 * Where this session stands, in a sentence, derived from the newest turn
 * that carries one. Absent until a turn has been recapped, and on
 * machines with no utility model to derive one.
 */
recap?: string, };

/**
 * Identifies one durable conversation with an external agent engine.
 */
export type CodeSessionId = string;

/**
 * Why a session exists: the user's conversation, or an automation task.
 */
export type CodeSessionKind = "interactive" | "watch";

/**
 * Lifecycle of a persisted code session.
 */
export type CodeSessionLifecycle = "created" | "idle" | "running" | "fenced" | "ended";

/**
 * One durable conversation with an external agent engine.
 */
export type CodeSessionSnapshot = { id: CodeSessionId, workspace_id: WorkspaceId, kind: CodeSessionKind, harness_kind: HarnessKind, harness_version?: string, harness_resume_ref?: string, permission_mode: PermissionMode, model?: string, 
/**
 * Absent means the engine's own default, which is not any level.
 */
reasoning_effort?: ReasoningEffort, 
/**
 * Whether this session runs its turns in the engine's fast mode.
 */
fast_mode: boolean, lifecycle: CodeSessionLifecycle, fence_reason?: FenceReason, attention: Attention, unrecognized_event_count: number, created_at: string, };

/**
 * The next reclaim tier a workspace can take.
 */
export type CodeStorageAction = "archive" | "release";

/**
 * Reclaimable disk for every repo and workspace the principal owns.
 */
export type CodeStorageSnapshot = { repos: Array<CodeRepoStorageSnapshot>, };

/**
 * Status of a harness subagent, derived from its spanning `Task` call
 * (decision 52): the call's start is the subagent's start, its result is
 * the end and outcome.
 */
export type CodeSubagentStatus = "running" | "done" | "failed";

/**
 * One harness subagent on a session, tracked for rail visibility. Not a
 * session: the harness owns its lifecycle, so the server can neither steer
 * nor resume it (decision 52).
 */
export type CodeSubagentSummary = { 
/**
 * The spanning `Task` call's engine-native id.
 */
call_id: string, 
/**
 * Display name: the Task's description or the tool name.
 */
name: string, 
/**
 * Status derived from the spanning call.
 */
status: CodeSubagentStatus, };

/**
 * Unsequenced activity notice published on the updates channel.
 *
 * Never journaled. A client that missed one just pulls from its last cursor.
 */
export type CodeTerminalActivityNotice = { workspace_id: WorkspaceId, terminal_id: CodeTerminalId, };

/**
 * Identifies one auxiliary terminal attached to a workspace.
 */
export type CodeTerminalId = string;

/**
 * Cursor-pull response for `GET /code/workspaces/{id}/terminals/{tid}/read`.
 *
 * `bytes` is standard base64 of the raw ring slice. `overflow` is true when
 * the requested cursor had already fallen out of the ring; the payload then
 * starts with the inline truncation marker.
 */
export type CodeTerminalRead = { id: CodeTerminalId, workspace_id: WorkspaceId, bytes: string, cursor: number, overflow: boolean, truncated: boolean, ended: boolean, };

/**
 * One live auxiliary terminal. Bytes live only in the process ring.
 */
export type CodeTerminalSnapshot = { id: CodeTerminalId, workspace_id: WorkspaceId, cols: number, rows: number, ended: boolean, created_at: string, };

/**
 * What a trigger does when its condition fires.
 *
 * Two actions in v1. Merge, auto-merge, and mark-ready stay with the user
 * (decision 42), and shell commands and webhooks need their own record.
 */
export type CodeTriggerAction = "deliver" | "notify";

/**
 * A pull-request fact a trigger fires on.
 *
 * The vocabulary is the watch classifier's, minus the states nothing can act
 * on. A trigger fires on the *transition* into one of these, once per head
 * SHA — see [`CodeTriggerFire`] (decision 60).
 */
export type CodeTriggerCondition = "checks_failed" | "conflicts" | "changes_requested" | "review_required" | "behind" | "ready_to_merge" | "merged" | "closed" | "pr_opened" | "pr_updated";

/**
 * Identifies one durable trigger rule bound to a repository.
 */
export type CodeTriggerId = string;

/**
 * One armed trigger, as the interface reads it.
 */
export type CodeTriggerSnapshot = { id: CodeTriggerId, repo_id: RepoId, condition: CodeTriggerCondition, action: CodeTriggerAction, enabled: boolean, created_at: string, updated_at: string, };

/**
 * Identifies one user→engine cycle inside a code session.
 */
export type CodeTurnId = string;

/**
 * Where one closing-message rewrite stands.
 */
export type CodeTurnRewriteState = "rewriting" | "rewritten" | "failed";

/**
 * One user→engine turn.
 */
export type CodeTurnSnapshot = { id: CodeTurnId, session_id: CodeSessionId, ordinal: number, status: CodeTurnStatus, model?: string, fast_mode: boolean, user_input: string, attachments: Array<ImageRef>, usage?: CodeUsage, checkpoint_ref?: string, diffstat?: Diffstat, started_at: string, ended_at?: string, 
/**
 * Lucid rewrite of the closing message. The journal keeps the original.
 */
rewrite?: string, };

/**
 * Status of one user→engine turn.
 */
export type CodeTurnStatus = "running" | "completed" | "failed" | "interrupted";

/**
 * One unsequenced notice on `WS /code/updates`.
 *
 * A connect is restated as [`Self::Snapshot`]; later notices are live only.
 */
export type CodeUpdateNotice = { "type": "snapshot", 
/**
 * One row per live session.
 */
sessions: Array<CodeSessionDigest>, } | { "type": "digest", workspace: WorkspaceId, session: CodeSessionId, kind: CodeSessionKind, 
/**
 * Engine identity for the session represented by this digest.
 */
harness_kind?: HarnessKind, lifecycle: CodeSessionLifecycle, attention: Attention, title: string, turn_count: number, 
/**
 * What the live turn is occupied with, while running.
 */
activity?: CodeSessionActivity, 
/**
 * Boxed to keep the notice enum's variants near one size; the wire
 * shape is unchanged.
 */
pr_state?: PullRequestDigest, 
/**
 * How many pull requests hold a durable attribution to this
 * workspace (decision 77). Absent when none do.
 */
pr_count?: number, 
/**
 * Watch progress, present only on `kind: watch` digests.
 */
watch_state?: CodeWatchState, watch_detail?: string, watch_cycles?: number, 
/**
 * Harness subagents on this session, present only when any were
 * observed (decision 52).
 */
subagents?: Array<CodeSubagentSummary>, 
/**
 * Where this session stands, in a sentence, derived from the newest
 * turn that carries one.
 */
recap?: string, } | { "type": "terminal_activity", workspace_id: WorkspaceId, terminal_id: CodeTerminalId, } | { "type": "clone_progress", job: string, phase: string, percent?: number, done: boolean, error?: string, repo_id?: RepoId, } | { "type": "harness_install", kind: HarnessKind, version?: string, phase: string, done: boolean, error?: string, } | { "type": "delivery" } | { "type": "turn_rewrite", session: CodeSessionId, turn_id: CodeTurnId, state: CodeTurnRewriteState, rewrite?: string, };

/**
 * Token accounting for one turn.
 *
 * The four counts are **disjoint** and they are **turn totals**, summed over
 * every model call the engine made while servicing the turn. This is the same
 * contract the chat side states on `RendererTurnUsage`, and the reason to
 * state it here is that every adapter reports something different natively:
 * one engine sends a running total beside the last call's slice, another
 * folds the cached portion back into the prompt count, another overwrites a
 * snapshot per message. Normalizing belongs in the adapter, so that anything
 * reading this struct — cost accounting, the CLI's turn list, the desktop's
 * context indicator — can compare two harnesses without knowing which engine
 * produced the row.
 *
 * Concretely: `input_tokens` is the *fresh*, uncached prompt only. It never
 * includes `cache_read_input_tokens` or `cache_creation_input_tokens`, so
 * the prompt an engine actually sent is the sum of all three. Missing fields
 * stay zero, which is not the same as "the engine sent zero" — an engine that
 * does not surface cache counts reports nothing rather than a real zero.
 *
 * None of the four answers "how full is the window". Summing turn totals
 * counts the same transcript once per model call, so a long turn reads as a
 * multiple of the prompt that was actually resident. That reading has its own
 * field: [`CodeUsage::context_tokens`].
 */
export type CodeUsage = { 
/**
 * Fresh, uncached input tokens. Excludes both cache fields.
 */
input_tokens: number, 
/**
 * Output tokens, summed over the turn's model calls.
 */
output_tokens: number, 
/**
 * Cache-read input tokens, when the engine reports them.
 */
cache_read_input_tokens: number, 
/**
 * Cache-write input tokens, when the engine reports them.
 */
cache_creation_input_tokens: number, 
/**
 * Prompt tokens resident on the turn's final model call — what actually
 * occupied the context window at the end of the turn.
 *
 * Distinct from the four counts above, which are the turn's *spend*
 * summed across every model call. On a six-call turn those sum to
 * roughly six prompts; this is the one prompt that was live when the
 * turn ended, and it is the only honest numerator for "how full is the
 * window".
 *
 * Zero when the engine does not publish enough to compute it.
 */
context_tokens: number, 
/**
 * Prompt tokens resident on the turn's first model call, when the engine
 * publishes per-call usage. This exposes startup context separately from
 * context that grows while the turn runs.
 */
first_call_context_tokens?: number, };

/**
 * Identifies one durable watch task on a workspace's pull request.
 */
export type CodeWatchId = string;

/**
 * One durable watch task on a workspace's pull request.
 */
export type CodeWatchSnapshot = { id: CodeWatchId, workspace_id: WorkspaceId, session_id: CodeSessionId, pr_number: number, state: CodeWatchState, detail?: string, cycles: number, created_at: string, updated_at: string, };

/**
 * Bounded image reference recorded on a code-mode user turn.
 *
 * State of a persisted watch task.
 */
export type CodeWatchState = "watching" | "fixing" | "blocked" | "done" | "stopped" | "failed";

/**
 * One worktree file's text for the center viewer.
 */
export type CodeWorkspaceBlob = { path: string, content: string, truncated: boolean, binary: boolean, };

/**
 * Bounded unified diff for `GET /code/workspaces/{id}/diff`.
 */
export type CodeWorkspaceDiff = { diff: string, truncated: boolean, stat: Diffstat, turn_id?: CodeTurnId, file?: string, };

/**
 * Bounded changed-file list for `GET /code/workspaces/{id}/files`.
 */
export type CodeWorkspaceFiles = { files: Array<CodeFileChange>, truncated: boolean, stat: Diffstat, turn_id?: CodeTurnId, };

/**
 * One repository-wide conversation-history match.
 */
export type CodeWorkspaceHistorySearchMatch = { workspace_id: WorkspaceId, workspace_title: string, session_id: CodeSessionId, turn_id?: CodeTurnId, source: CodeWorkspaceHistorySearchSource, preview: string, created_at: string, };

/**
 * Stored field that produced a workspace conversation-history match.
 */
export type CodeWorkspaceHistorySearchSource = "turn_user_input" | "turn_narrative" | "event";

/**
 * PR + checks digest plus the local git facts the PR card needs.
 */
export type CodeWorkspacePrSnapshot = { dirty: boolean, unpushed: boolean, ahead: number, has_upstream: boolean, suggested_commit_message: string, pr?: PullRequestDigest, gh_found: boolean, gh_authenticated?: boolean, remediation: string, 
/**
 * The identity a push from this machine acts as: the deployment's
 * GitHub App bot account (decision 63) or the caller's own login
 * (decision 65). The UI states this plainly beside the push control.
 */
pushes_as?: string, 
/**
 * Whether `pushes_as` is the caller's own account (decision 65)
 * rather than the deployment's App.
 */
pushes_as_self?: boolean, watch?: CodeWatchSnapshot, };

/**
 * One pull request attributed to a workspace, from the durable fact store
 * (decision 77). A projection of the stored snapshot — no live host read.
 */
export type CodeWorkspacePullRequestFact = { host: string, repo_owner: string, repo_name: string, number: number, url: string, title: string, 
/**
 * Coarse lifecycle: `open`, `merged`, or `closed`.
 */
state: string, draft: boolean, author?: string, head_branch: string, base_branch: string, head_sha?: string, 
/**
 * How the workspace is tied to it.
 */
relation: CodePullRequestRelation, created_at: string, updated_at: string, merged_at?: string, closed_at?: string, 
/**
 * When the store last confirmed this snapshot against the host.
 */
last_seen_at: string, };

/**
 * Response of `GET /code/workspaces/{id}/pull-requests`: every pull request
 * this workspace authored or contributed to, open first, newest first.
 */
export type CodeWorkspacePullRequests = { items: Array<CodeWorkspacePullRequestFact>, fetched_at: string, };

/**
 * Bounded content-search response for `GET /code/workspaces/{id}/search`.
 */
export type CodeWorkspaceSearch = { matches: Array<CodeWorkspaceSearchMatch>, history_matches?: Array<CodeWorkspaceHistorySearchMatch>, truncated: boolean, };

/**
 * One matching line from a workspace content search.
 */
export type CodeWorkspaceSearchMatch = { path: string, line_number: number, line: string, };

/**
 * One isolated workspace (worktree + branch) on a repo.
 */
export type CodeWorkspaceSnapshot = { id: WorkspaceId, repo_id: RepoId, title: string, worktree_path: string, branch_name: string, base_ref: string, status: CodeWorkspaceStatus, pr?: PullRequestDigest, created_at: string, archived_at?: string, released_at?: string, 
/**
 * Commit the released branch pointed at, so a client can name the work
 * without the branch existing.
 */
released_tip?: string, 
/**
 * Stored bundle size, for reporting what a release reclaimed.
 */
bundle_bytes?: number, };

/**
 * Status of a persisted workspace.
 */
export type CodeWorkspaceStatus = "creating" | "setup_failed" | "active" | "archiving" | "archived" | "released";

/**
 * One workspace's current footprint and the next reclaim step.
 */
export type CodeWorkspaceStorageSnapshot = { id: WorkspaceId, title: string, status: CodeWorkspaceStatus, on_disk_bytes: number, next_action?: CodeStorageAction, next_reclaim_bytes: number, };

/**
 * Bounded path listing for `GET /code/workspaces/{id}/tree`.
 *
 * Paths only. Never file contents.
 */
export type CodeWorkspaceTree = { paths: Array<string>, truncated: boolean, };

/**
 * Where new worktrees land: `GET`/`PUT /code/worktree-root`.
 *
 * `root` is the stored setting and is absent while the deployment runs on its
 * default. `effective_root` is what the next workspace uses, and
 * `default_root` is what clearing the setting returns to — so a reader can
 * tell a chosen path from an inherited one without repeating the rule.
 */
export type CodeWorktreeRoot = { root?: string, effective_root: string, default_root: string, };

/**
 * What one on-demand compaction did.
 */
export type CompactionRun = { 
/**
 * Whether a checkpoint was written. `false` is a complete, ordinary answer:
 * the chat had too little history to give up, its recent messages are all
 * protected, or the summarizer declined. Nothing is wrong, and nothing
 * changed — the caller says so rather than leaving the reader with silence.
 */
compacted: boolean, };

/**
 * Host-tunable chat compaction cadence and retention.
 */
export type CompactionSettings = { 
/**
 * Compact when unabridged tokens exceed this fraction of the context window.
 */
threshold_fraction: number, 
/**
 * After compaction, keep about this fraction of the window as raw recent history.
 */
target_fraction: number, 
/**
 * Absolute floor applied before scaling by context window.
 */
min_threshold_tokens: number, 
/**
 * Newest durable messages that must never enter the compacted prefix.
 */
protect_recent_messages: number, };

/**
 * Identifies one profile-scoped connected app — an outside integration
 * (an MCP server, a REST API) a profile can reach.
 *
 * App-keyed manifest bindings and grants name this identity rather than a
 * raw server namespace, so consent follows the record even when display
 * names or namespaces change around it.
 */
export type ConnectedAppId = string;

/**
 * One connected app, projected per kind for the Settings listing.
 *
 * Closed and renderer-safe: an `mcp_server` entry carries the runtime's
 * health projection, a `rest_api` entry carries catalog and credential
 * *status* — never transport definitions, documents, or values.
 */
export type ConnectedAppInfo = { "kind": "mcp_server", 
/**
 * The record id app bindings name.
 */
id: ConnectedAppId, 
/**
 * Display name — also the namespace the server's tools mount under.
 */
name: string, health: McpHealth, tool_count: number, 
/**
 * The bare mounted tool names (after the `mcp__{server}__` prefix),
 * bounded by the per-server discovery cap. Names only — never
 * remote-authored descriptions — the consent sheet's posture.
 */
tools: Array<string>, diagnostic: string | null, 
/**
 * The curated-list entry this server matched, when Tidebreak has
 * exercised it end to end. `null` is the community tier: mounted and
 * usable, just not something we have driven ourselves. A label, not
 * a gate — nothing about the mount changes either way.
 */
curated: McpCuration | null, 
/**
 * The gateway MCP endpoint slug this record mounts, when it is
 * gateway-backed rather than a local stdio/HTTP definition.
 */
gateway_endpoint: string | null, 
/**
 * Display names of the organization's entitled apps that ride this
 * record's gateway endpoint. Empty for local records — and, by
 * graceful degradation, when the gateway is unreachable or predates
 * the apps surface: the entry then renders without org-app names
 * rather than erroring.
 */
gateway_apps: Array<string>, 
/**
 * How many local mini-apps hold a live grant binding this record —
 * a count only, never app names or ids, the renderer-safety posture
 * of this surface. Grants of library-deleted apps do not count.
 */
used_by_app_count: number, } | { "kind": "rest_api", 
/**
 * The record id app bindings name.
 */
id: ConnectedAppId, name: string, base_url: string, 
/**
 * Operations the ingested catalog declares.
 */
operation_count: number, 
/**
 * Hex SHA-256 of the raw OpenAPI document the catalog was ingested
 * from.
 */
document_sha256: string, credential_status: RestCredentialStatus, 
/**
 * Where the stored credential value is placed at request time, when
 * one is referenced. The placement (and a custom header *name*) is
 * configuration; the value never appears on this surface.
 */
placement: CredentialPlacement | null, updated_at: string, 
/**
 * How many local mini-apps hold a live grant binding this record —
 * a count only, never app names or ids, the renderer-safety posture
 * of this surface. Grants of library-deleted apps do not count.
 */
used_by_app_count: number, };

/**
 * The renderer's listing of every configured connected app, across kinds.
 */
export type ConnectedAppsInfo = { 
/**
 * MCP entries in the runtime's configuration order, then REST entries in
 * storage order (oldest first).
 */
apps: Array<ConnectedAppInfo>, };

/**
 * The durable identity a revocation names.
 *
 * The two stores revoke differently: a tool grant is withdrawn through
 * `DELETE /grants/{call_id}`, while a capability grant is not individually
 * revocable yet — today the Folders surface disconnects the whole root, and
 * per-statement withdrawal arrives when the boundary is derived from these
 * statements.
 */
export type ConsentHandle = { "kind": "tool_grant", call_id: CallId, } | { "kind": "capability_grant", grant_id: string, };

/**
 * The trusted interaction that captured a consent statement.
 */
export type ConsentMethodSnapshot = "approval_card" | "folder_picker" | "trusted_folder" | "permission_dialog" | "operator_config" | "carried_forward";

/**
 * What a consent statement's verb is allowed to touch.
 */
export type ConsentResource = { "kind": "action_scope", scope: GrantScope, } | { "kind": "host_subject" } | { "kind": "host_root", root_id: string, display_name: string | null, } | { "kind": "host_path_subtree", root_id: string, display_name: string | null, relative: string, };

/**
 * One statement of consent the agent currently holds, whatever store it
 * lives in.
 */
export type ConsentStatementSnapshot = { 
/**
 * What a revocation of this statement names, and where to send it.
 */
handle: ConsentHandle, 
/**
 * How far the statement reaches — one chat, or every chat in a project.
 */
level: GrantLevel, 
/**
 * The name of whatever the level points at, for provenance. `None` when
 * that chat or project is untitled.
 */
level_title: string | null, 
/**
 * The class of action the user allowed.
 */
verb: ConsentVerb, 
/**
 * What the verb is allowed to touch.
 */
resource: ConsentResource, 
/**
 * The trusted interaction through which consent was captured.
 */
method: ConsentMethodSnapshot, granted_at: string, };

/**
 * The class of action a consent statement allows.
 */
export type ConsentVerb = { "kind": "tool", action: RendererToolName, approval: ToolApprovalKind, } | { "kind": "capability", capability: HostCapability, };

/**
 * Arm a trigger on a repository.
 */
export type CreateCodeTriggerBody = { condition: CodeTriggerCondition, action: CodeTriggerAction, };

/**
 * Where the resolved credential value is placed on the request.
 *
 * Externally tagged and closed: `"bearer"` or `{"header": "X-Api-Key"}`; an
 * unknown variant refuses to deserialize. A named header must be a valid
 * header token and may name `Authorization` explicitly, but never a header
 * the executor owns or that alters routing (see [`RestExecuteError::ForbiddenHeader`]).
 */
export type CredentialPlacement = "bearer" | { "header": string };

/**
 * User-inspectable routing limits and capabilities for one configured model.
 *
 * OpenAI-compatible rows are validated to the conservative text-only shape.
 * xAI rows may opt into the capabilities its first-party Responses adapter
 * actually carries end to end.
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
 * The provider-side id a managed gateway routes this model to, when the
 * gateway reports one that differs from `id`.
 *
 * Populated only by gateway model sync — a user-entered custom model
 * leaves it unset, and nothing in the settings UI offers it. It exists so
 * a deployment-aliased id can still be recognized as a curated model.
 */
upstream_id?: string | null, 
/**
 * Alternate gateway ids that also resolve to this model — the
 * deployment-shaped spellings the member catalog reports, offered to
 * the curated registry the same way `upstream_id` is.
 *
 * Populated only by gateway model sync from a catalog-serving gateway;
 * a user-entered custom model leaves it empty.
 */
aliases?: Array<string>, 
/**
 * Context limit used by Tidebreak's reducer.
 */
context_window: number, 
/**
 * Maximum output sent to the endpoint.
 */
max_output_tokens: number, 
/**
 * Inputs Tidebreak may place on this model's request.
 */
input_modalities: Array<InputModality>, 
/**
 * Whether the model uses xAI's reasoning request shape.
 */
supports_reasoning: boolean, 
/**
 * Reasoning-effort levels accepted by the model, ascending.
 */
reasoning_efforts: Array<ReasoningEffort>, };

/**
 * Wire mirror of the admission gate's typed denial reasons
 * ([`crate::sandbox_admission::DetachedAdmissionDenial`]), so the renderer
 * maps each to user-facing language instead of receiving prose the server
 * composed.
 */
export type DetachedAdmissionDenialReason = "no_scoped_model_token" | "no_external_lifetime_cap" | "image_not_verified" | "host_authority_tool_surface" | "credentials_without_external_egress";

/**
 * One provider's detached-admission verdict, renderer-safe.
 *
 * `denials` is what the real evaluator returned for this provider's declared
 * capabilities: empty exactly when `admitted`. The rows exist even for
 * providers that cannot host background runs at all — every precondition is
 * simply unestablished for them, and the fail-closed evaluation names each.
 */
export type DetachedAdmissionProviderInfo = { provider: ExecProviderKind, 
/**
 * Whether the gate would admit a detached run hosted by this provider.
 */
admitted: boolean, 
/**
 * Every unmet precondition, named — not just the first.
 */
denials: Array<DetachedAdmissionDenialReason>, };

/**
 * Bounded add/delete counts for a diff. Bodies live on a GET route.
 */
export type Diffstat = { 
/**
 * Files touched.
 */
files: number, 
/**
 * Lines added.
 */
insertions: number, 
/**
 * Lines deleted.
 */
deletions: number, 
/**
 * True when the underlying diff was truncated.
 */
truncated: boolean, };

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
export type EgressEnforcementStatus = "boundary" | "conditional_boundary" | "applied_with_gaps" | "unconfirmed" | "not_enforced";

/**
 * The execution backend that ran a command, as a closed vocabulary.
 *
 * Read through this enum rather than surfaced as a string, on the same terms
 * as [`ExecDegradation`]: the card names the backend, and the card's words
 * are written on this side. A backend the renderer does not know projects as
 * nothing rather than as passthrough text.
 */
export type ExecBackend = "local" | "e2b" | "daytona" | "docker";

/**
 * Renderer-safe configuration and readiness.
 */
export type ExecConfigInfo = { provider?: ExecProviderKind, timeout_ms: number, available: boolean, 
/**
 * Why the *selected* provider cannot run, when it cannot. Absent while
 * execution is available or no provider is selected at all.
 */
unavailable_reason?: ExecUnavailableReason, has_credential: boolean, 
/**
 * One row per shipped provider: whether it could run here at all, and the
 * reason it could not. This is what makes an unusable host legible —
 * "paste an E2B key" is visible instead of being inferred from a generic
 * execution failure.
 */
providers: Array<ExecProviderAvailability>, 
/**
 * The configured egress policy and each managed provider's enforcement
 * status, so the renderer can present the policy and disclose which
 * providers actually restrict egress today.
 */
egress: ExecEgressInfo, 
/**
 * Per-provider detached-admission evaluation: for each execution
 * provider, whether the fail-closed gate (issue #824) would admit a
 * detached run it hosted, and every named precondition it fails. Derived
 * by running the real admission evaluator over each provider's declared
 * capabilities — the settings surface and the gate cannot disagree.
 */
detached_admission: Array<DetachedAdmissionProviderInfo>, };

/**
 * Renderer-safe readiness for one managed provider's fixed credential slot.
 */
export type ExecCredentialReadiness = { provider: ExecProviderKind, has_credential: boolean, };

/**
 * A way the execution backend ran with less than its intended setup.
 *
 * Closed, and deliberately coarse: what a reader needs is what happened and
 * what it costs them, not which vendor API returned which status. A provider
 * that degrades a second way earns a second variant here rather than a free
 * string, because the sentence the card shows is written on this side.
 */
export type ExecDegradation = "sandbox_image_unavailable";

/**
 * A managed provider's egress-enforcement status, as host knowledge rather
 * than a claim the backend makes about itself.
 */
export type ExecEgressEnforcement = { provider: ExecProviderKind, status: EgressEnforcementStatus, 
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
export type ExecEgressInfo = { 
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
enforcement: Array<ExecEgressEnforcement>, };

/**
 * Renderer-safe selection metadata; bytes remain behind the scoped endpoint.
 */
export type ExecFileBinaryPreview = { format: ExecFilePreviewFormat, before: ExecFilePreviewAvailability, after: ExecFilePreviewAvailability, };

/**
 * Renderer-owned classification of one journal row.
 */
export type ExecFileChangeClassification = "applied" | "rejected";

/**
 * The successful filesystem effect, absent for a rejected write.
 */
export type ExecFileChangeKind = "created" | "overwritten" | "deleted";

/**
 * One renderer-safe row in a terminal turn's file-change summary, including a
 * bounded unified diff when both revisions are text or a binary preview
 * selector whose bytes remain behind the scoped preview endpoint.
 */
export type ExecFileChangeSummary = { snapshot_id: string, folder_name: string, relative_path: string, classification: ExecFileChangeClassification, change: ExecFileChangeKind | null, rejection_reason: ExecFileRejectionReason | null, undo: ExecFileUndoAvailability, diff: string | null, binary_preview: ExecFileBinaryPreview | null, };

/**
 * Whether one side of a binary before/after comparison can be requested.
 */
export type ExecFilePreviewAvailability = "available" | "empty" | "stale" | "too_large" | "unavailable";

/**
 * A binary format handled by the bundled #1056 document renderer.
 */
export type ExecFilePreviewFormat = "pdf" | "docx" | "xlsx";

/**
 * Why one staged file was left out of the user's folder.
 */
export type ExecFileRejectionReason = "stale" | "snapshot_unavailable" | "staged_file_too_large" | "trash_unavailable" | "unavailable";

/**
 * Whether an applied file can still be safely reverted now.
 */
export type ExecFileUndoAvailability = "available" | "already_undone" | "stale" | "not_available";

/**
 * Structured capability report for one execution provider on this host.
 *
 * `available` and `unavailable_reason` are two views of one decision, made in
 * [`provider_availability`], so no surface has to re-derive whether a platform
 * supports a provider or whether a key is saved.
 */
export type ExecProviderAvailability = { provider: ExecProviderKind, available: boolean, unavailable_reason?: ExecUnavailableReason, };

/**
 * A configured code-execution backend.
 */
export type ExecProviderKind = "local" | "e2b" | "daytona" | "docker";

/**
 * Host-selected backend that runs `exec` tool calls.
 *
 * Distinct from [`AgentRunExecutionLocation`], which names where the agent
 * *run loop* itself executes (`in_process` vs `container`). A background run
 * can be in-process while its shell work still lands on e2b, docker, or
 * daytona — this field is that backend, or `off` when code execution is
 * disabled.
 */
export type ExecProviderSnapshot = "local" | "e2b" | "daytona" | "docker" | "off";

/**
 * Why a provider cannot execute anything on this host right now.
 *
 * A stable machine-readable code, not a sentence: the reason is decided where
 * the fact is known (the platform probe, the credential slot) and every
 * surface renders its own copy from the code. Reasons are what the user can
 * act on — install a key, switch provider — never an internal failure detail.
 */
export type ExecUnavailableReason = "unsupported_platform" | "missing_sandbox_binary" | "missing_credential" | "missing_container_runtime" | "container_runtime_unreachable";

/**
 * Why a session is fenced: observed but not controlled, until an explicit
 * user reap resolves it.
 */
export type FenceReason = { "type": "orphan_alive" } | { "type": "probe_ambiguous", 
/**
 * Bounded human-readable detail.
 */
detail: string, } | { "type": "resume_lost", 
/**
 * Bounded human-readable detail, as the engine reported it.
 */
detail: string, } | { "type": "repeated_turn_failures", 
/**
 * How many turns failed in a row.
 */
count: number, 
/**
 * Bounded detail from the last failure, as the engine reported it.
 */
detail: string, } | { "type": "incarnation_unresolved", 
/**
 * Bounded human-readable detail.
 */
detail: string, } | { "type": "sandbox_lost", 
/**
 * Bounded detail, as the environment classified it.
 */
detail: string, } | { "type": "terminal_flush_missing", 
/**
 * Bounded human-readable detail.
 */
detail: string, };

/**
 * Kind of file change reported by the engine or a checkpoint.
 */
export type FileChangeKind = "added" | "modified" | "deleted" | "renamed";

/**
 * The access level of a folder binding.
 *
 * Consent-bearing: the level is part of what the user grants and part of
 * the binding's fingerprint, so widening `read` to `read_write` always
 * re-prompts.
 * The access level of a folder binding.
 *
 * Consent-bearing: the level is part of what the user grants and part of
 * the binding's fingerprint, so widening `read` to `read_write` always
 * re-prompts.
 */
export type FolderAccess = "read" | "read_write";

/**
 * One entitled connected app, with the slugs of the MCP endpoints that
 * aggregate it — the `mcp:<slug>` resources a mount would request.
 */
export type GatewayAppInfo = { id: string, name: string, app_kind: string, enabled: boolean, mcp_endpoint_slugs: Array<string>, 
/**
 * The gateway's readiness for this app when the member catalog reports
 * one: `ready`, `not_connected`, or `authorization_required`. `None`
 * against a gateway that predates the catalog — the panel then shows
 * no readiness rather than guessing. An unfamiliar value renders as
 * not-ready copy, never an error: the set is the gateway's to grow.
 */
connection?: string, 
/**
 * How many live local-app grants bind this gateway app — the same
 * "Used by N local apps" line the connected-apps page carries per record,
 * so a user can see what a revocation here would break.
 */
used_by_app_count: number, };

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
export type GatewayStatus = { base_url?: string, signed_in: boolean, account_hint?: string, installation_id?: string, model_count: number, 
/**
 * The member-catalog contract revision the last model sync read, or
 * `None` while unsynced or against a gateway that predates
 * `/api/v1/me/catalog`. The settings panel uses its absence (while
 * signed in with models) to note that the deployment is older than
 * this Tidebreak.
 */
member_catalog?: string, sign_in: SignInProgress, };

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
export type GrantScope = { "scope": "exact_action" } & ToolActionPreview | { "scope": "any_args_for", command: string, } | { "scope": "command_prefix", tokens: Array<string>, } | { "scope": "path_subtree", prefix: string, } | { "scope": "whole_tool" };

/**
 * How a session of one engine authenticates on this machine.
 */
export type HarnessAuthMode = "local_sign_in" | "gateway_managed" | "gateway_relay" | "hosted_unavailable";

/**
 * Capability vector for one probed engine version.
 *
 * Constructed exhaustively — there is no [`Default`] — so a new flag is a
 * compile break at every adapter.
 */
export type HarnessCaps = { 
/**
 * The engine can resume a prior session from a native resume ref.
 */
resume: CapLevel, 
/**
 * The engine streams partial assistant text.
 */
streaming_deltas: CapLevel, 
/**
 * The engine exposes a structured approval channel.
 */
structured_approvals: CapLevel, 
/**
 * The engine accepts a mid-turn user message.
 */
mid_turn_steering: CapLevel, 
/**
 * The engine has a read-only / plan posture the adapter can select.
 */
plan_mode: CapLevel, 
/**
 * The engine has a workspace-write / auto posture the adapter can select.
 *
 * Whether that posture is supervised is a separate question:
 * with `structured_approvals` supported, sensitive actions still park on
 * approval cards; without it, Auto runs unsupervised and the product
 * states so where the mode is chosen (decision 0038).
 */
auto_mode: CapLevel, 
/**
 * The engine has an allow-everything / bypass posture the adapter can
 * select. Composed only when the session is in Allow (decision 0039).
 */
allow_mode: CapLevel, 
/**
 * The engine accepts a reasoning-effort control.
 */
reasoning_levels: CapLevel, 
/**
 * The engine emits native file-change events.
 */
native_file_change_events: CapLevel, 
/**
 * The engine honors a native interrupt.
 */
native_interrupt: CapLevel, 
/**
 * The engine consumes an image on its machine-readable input path.
 *
 * Adapters may only state [`CapLevel::Supported`] with a fixture that
 * proves a captured image round-trip. The product must not offer
 * attachments otherwise.
 */
image_input: CapLevel, 
/**
 * The engine has a discoverable slash-command vocabulary.
 *
 * Independent of whether free-typed `/` text is accepted — that is
 * always pass-through. This flag only says a machine-readable listing
 * exists to feed the composer popup.
 */
slash_commands: CapLevel, };

/**
 * One engine-owned slash command, captured from the engine's own listing.
 */
export type HarnessCommand = { 
/**
 * The word typed after `/`. No leading slash.
 */
name: string, 
/**
 * One-line description from the engine, already bounded.
 */
description: string, };

/**
 * One engine's probe, capabilities, and remediation.
 */
export type HarnessDoctorEntry = { kind: HarnessKind, found: boolean, 
/**
 * Whether Tidebreak ships a pin it can download for this engine.
 *
 * A `found: false, installable: true` engine is not a fault. Pick it and
 * the download starts; the doctor is not a gate the reader must clear
 * first.
 */
installable: boolean, path?: string, version?: string, tier: HarnessTier, caps: HarnessCaps, commands: Array<HarnessCommand>, authenticated?: boolean, 
/**
 * How a session of this engine authenticates here: the engine's own
 * local sign-in, a credential override that authenticates without one
 * (issue 2749), the on-behalf-of relay on a gateway-hosted machine
 * (decision 71), or nothing where the relay does not cover the engine
 * yet. Readiness follows this — `authenticated` stays the local probe
 * observation, which on a hosted or gateway-managed machine is not what
 * a session authenticates with.
 */
auth_mode: HarnessAuthMode, remediation: string, stderr: string, unrecognized_event_count: number, 
/**
 * Whether relaunching a session applies a permission mode chosen after
 * it started. False for engines that fix the mode on session create
 * (opencode); true where a relaunch rebuilds the launch plan.
 */
relaunch_composes_permission_mode: boolean, };

/**
 * Doctor report for every registered engine adapter.
 */
export type HarnessDoctorReport = { harnesses: Array<HarnessDoctorEntry>, };

/**
 * Which external agent engine a session is bound to.
 *
 * Named after the shipped adapters. The traits those adapters implement
 * stay engine-neutral; this enum is only the catalog of known engines.
 */
export type HarnessKind = "claude_code" | "codex" | "opencode" | "grok";

/**
 * One model row offered for a harness session.
 */
export type HarnessModel = { id: string, label: string, default: boolean, 
/**
 * Effort levels this row accepts, ascending. Empty hides the control.
 */
reasoning_efforts: Array<ReasoningEffort>, 
/**
 * Whether this row can serve the engine's fast mode. `false` hides the
 * control, the same way an empty effort ladder does.
 */
fast_mode: boolean, };

/**
 * `GET /code/harnesses/{kind}/models`.
 */
export type HarnessModelList = { kind: HarnessKind, models: Array<HarnessModel>, 
/**
 * Every effort level this engine accepts, ascending, across all models.
 *
 * The outer bound, for a client holding a model row this list does not
 * contain — a gateway catalog row, or a session still on a model the
 * engine has since dropped. A row's own `reasoning_efforts` is narrower
 * and wins where it exists. Empty means the engine takes no effort
 * control at all.
 */
reasoning_efforts: Array<ReasoningEffort>, source: HarnessModelSource, };

/**
 * Whose catalog produced a harness model list.
 */
export type HarnessModelSource = "harness" | "model_gateway";

/**
 * Severity of a visible-degradation notice.
 */
export type HarnessNoticeLevel = "info" | "warning" | "error";

/**
 * Adapter maturity, independent of any one capability flag.
 */
export type HarnessTier = "reference" | "secondary" | "tertiary" | "best_effort";

/**
 * Renderer-safe mirror of the host broker's capability vocabulary.
 *
 * The server does not link the broker crate, so the boundary vocabulary is
 * restated here for the wire; the desktop maps the broker's own enum into
 * this one when it assembles capability statements.
 */
export type HostCapability = "list_roots" | "read_files" | "write_files" | "execute_commands";

/**
 * Opaque identifier for a folder registered with a host broker.
 *
 * This is product projection data, not authority: possession of an id never
 * grants access to the corresponding host path. The broker independently
 * validates live attachments, consent, capabilities, and revocation.
 */
export type HostRootId = string;

/**
 * Image formats Tidebreak will send to a provider.
 *
 * Deliberately closed. Every variant here is accepted by the baseline
 * Anthropic and OpenAI image APIs. A provider with a narrower documented set
 * must refuse its unsupported variants before egress (xAI, for example,
 * accepts only PNG and JPEG) rather than passing them through to a provider
 * 400. Vector and exotic raster formats are excluded at the trusted ingest
 * boundary instead of being passed through at all.
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
 * Which conversation an entry belongs to.
 *
 * Tagged because chat ids and code session ids are still separate spaces
 * (the repository check `chat_and_code_entities_do_not_cross_reference`
 * enforces it). When step 5 merges the entities this collapses to one id.
 */
export type InboxConversation = { "surface": "chat", chat_id: ChatId, } | { "surface": "code", session_id: CodeSessionId, workspace_id: WorkspaceId, };

/**
 * One conversation that wants the reader, and why.
 */
export type InboxEntrySnapshot = { conversation: InboxConversation, 
/**
 * Absent, not null, while the conversation is still untitled.
 */
title?: string, attention: Attention, 
/**
 * The parked calls behind this attention, oldest first.
 *
 * Empty for a code conversation: its approvals are answered through the
 * code approval route, which carries the verbatim payload decision 33
 * requires and which a deep link reaches.
 */
items: Array<InboxItemSnapshot>, 
/**
 * When the oldest thing here started waiting, for ordering.
 */
waiting_since: string, };

/**
 * What kind of decision one inbox item is waiting for.
 *
 * The set is closed and each variant names an existing park/resume surface,
 * so a reader can route an item back to the card that owns it without the
 * inbox knowing anything about that card's contents.
 */
export type InboxItemKind = "tool_approval" | "question" | "plan_review" | "folder_access" | "output_writeback";

/**
 * One item waiting on the reader, and where to go to answer it.
 */
export type InboxItemSnapshot = { 
/**
 * With the entry's conversation, the deep link back to the exact
 * transcript position the item paused at.
 */
turn_id: TurnId, call_id: CallId, kind: InboxItemKind, 
/**
 * The tool under review, for an approval. Absent for the other kinds,
 * whose tool is implied by the kind. Closed renderer vocabulary: an
 * unrecognized name folds to `other` rather than reaching a card.
 */
action?: RendererToolName, requested_at: string, };

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
export type ManagedPolicy = { managed: boolean, gateway_url?: string, 
/**
 * The gateway a hosted deployment authenticates its own callers against
 * (`docs/decisions/0049-gateway-authenticated-hosted-machines.md`).
 *
 * Deliberately not [`ManagedPolicy::gateway_url`], and deliberately no
 * effect on `managed`. Those two say "this profile must hold a session
 * at this gateway", and that is false here: a caller authenticates *to*
 * a hosted machine with their own gateway token, never *through* it. A
 * hosted machine can therefore never report a session, so asserting
 * management over it would raise a sign-in gate nothing could ever
 * satisfy.
 *
 * This names the deployment for the surfaces that describe it, and
 * nothing else. Surfaces that lock a profile down read `managed`; a
 * hosted machine locks nothing, and its stored provider configuration
 * still wins (decision 51, rule 3).
 */
hosted_gateway_url?: string, source: ManagedPolicySource, 
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
pending_gateway_url?: string, 
/**
 * The highest permission mode any chat may run under, when the OS policy
 * asserts one. A ceiling, not a fixed mode: the reader may always pick a
 * stricter mode, and clearing back to the default is always allowed.
 * Asserted per key, so it binds even when no gateway URL is deployed and
 * the profile is otherwise unmanaged.
 */
permission_mode_ceiling?: PermissionMode, 
/**
 * True when the OS policy explicitly allows local stdio MCP servers on a
 * managed profile. False by default — the managed lockdown covers every
 * manual transport unless the organization opts in — and carries no
 * meaning while unmanaged, where nothing is locked to begin with.
 */
allow_local_mcp_servers: boolean, };

/**
 * Which authority asserted the active policy.
 */
export type ManagedPolicySource = "os" | "provisioned" | "unmanaged";

export type MarkNotificationsReadResult = { marked: number, };

/**
 * The curated-registry entry a configured server matched, projected to the
 * renderer beside the server's health.
 *
 * Presence *is* the tier: a server with a curation is "tested", a server
 * without one is "community". One field cannot disagree with itself the way
 * a separate boolean and a separate record could.
 */
export type McpCuration = { 
/**
 * The curated list's own name for the server, which need not match the
 * namespace the reader configured it under.
 */
display_name: string, 
/**
 * `YYYY-MM-DD` the entry was last exercised end to end.
 */
tested_on: string, 
/**
 * One sentence on what was exercised, for the reader deciding how much
 * the badge is worth.
 */
notes: string, };

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
 * Names of the environment variables this server is given directly. The
 * values live in the secret store under [`env_secret_key`] and never
 * enter this type, so they neither persist in the connected-app record
 * nor project through the API.
 */
env: Array<string>, 
/**
 * Inbound-only: values for [`env`](Self::env) names being set or
 * changed. A commit writes these into the secret store and drops them; a
 * name present in `env` but absent here keeps the value already stored,
 * which is what makes "leave blank to keep" work. `skip_serializing`
 * keeps them out of both the persisted record and every projection.
 */
env_values?: { [key in string]: string }, 
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
gateway_endpoint: string | null, request_timeout_ms: number, enabled: boolean, 
/**
 * The plugin this server was synthesized from, when it is plugin-sourced.
 *
 * Read-only over the API: `PUT /mcp/servers` refuses a body that sets it,
 * and the runtime rebuilds these entries from the installed plugin tree
 * rather than from anything a client sends or the store holds.
 */
plugin: string | null, };

/**
 * One renderer-safe server projection. Resolved `env_from` values and child
 * process details are intentionally absent.
 */
export type McpServerInfo = { health: McpHealth, tool_count: number, diagnostic: string | null, 
/**
 * The curated-list entry this definition matches, when Tidebreak has
 * exercised the server end to end. `null` means community: mounted and
 * usable, just not something we have driven ourselves. Derived from the
 * definition on every read, never stored.
 */
curated: McpCuration | null, name: string, command: string | null, args: Array<string>, 
/**
 * Names of the environment variables this server is given directly. The
 * values live in the secret store under [`env_secret_key`] and never
 * enter this type, so they neither persist in the connected-app record
 * nor project through the API.
 */
env: Array<string>, 
/**
 * Inbound-only: values for [`env`](Self::env) names being set or
 * changed. A commit writes these into the secret store and drops them; a
 * name present in `env` but absent here keeps the value already stored,
 * which is what makes "leave blank to keep" work. `skip_serializing`
 * keeps them out of both the persisted record and every projection.
 */
env_values?: { [key in string]: string }, 
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
gateway_endpoint: string | null, request_timeout_ms: number, enabled: boolean, 
/**
 * The plugin this server was synthesized from, when it is plugin-sourced.
 *
 * Read-only over the API: `PUT /mcp/servers` refuses a body that sets it,
 * and the runtime rebuilds these entries from the installed plugin tree
 * rather than from anything a client sends or the store holds.
 */
plugin: string | null, };

export type McpServersInfo = { servers: Array<McpServerInfo>, };

/**
 * Where the sandboxed iframe should load one view from, valid once.
 */
export type McpViewSession = { frame_path: string, };

/**
 * Body of `POST /code/workspaces/{id}/pr/merge`.
 */
export type MergeCodePrBody = { 
/**
 * The repository and pull request shown in the confirmation.
 */
target: CodeDeliveryPullRequestTarget, 
/**
 * The pull request head shown in the confirmation.
 */
expected_head_sha: string, method: CodePrMergeMethod, 
/**
 * True arms host auto-merge instead of merging immediately.
 */
auto: boolean, };

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
 * The vendor whose curated model this row is, when that differs from the
 * provider serving it — a gateway-served model whose id exactly matches a
 * curated one. For presentation only (icon and branding); routing still
 * uses `provider`, and a client falls back to it when this is null.
 */
vendor: ProviderKind | null, 
/**
 * How thoroughly Tidebreak has exercised this provider/model combination.
 */
verification: VerificationTier, 
/**
 * Whether a picker shows this model without being asked for the full
 * catalog — the curated default-visible set.
 *
 * Presentation only: a model that is not recommended is exactly as
 * selectable and supported as one that is. Effective visibility is this
 * flag flipped by any matching entry in the reader's
 * `model_visibility_overrides` setting; the server never filters the
 * catalog by it.
 */
recommended: boolean, 
/**
 * Whether the provider is enabled, configured, credentialed, and able to
 * serve this model at its configured endpoint/location.
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
 * Whether this provider/model route accepts function tools.
 */
supports_tools: boolean, 
/**
 * Whether this provider/model route can enforce the strict response schema
 * utility work depends on.
 */
supports_structured_output: boolean, 
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
 * A reader's explicit deviation from a model's curated `recommended` flag.
 *
 * Only deviations are stored. Effective visibility is the catalog's
 * `recommended` flag flipped by a matching override, so a catalog refresh
 * gives new models their curated default without a reconciliation step, and
 * "we changed the default" stays distinguishable from "you chose" forever.
 */
export type ModelVisibility = "show" | "hide";

/**
 * Network access granted to commands in one conversation workspace.
 *
 * The policy is provider-neutral. Providers compile it to their strongest
 * available enforcement mechanism; the local native adapter exposes only one
 * loopback broker port and applies the destination decision outside the
 * sandbox. Open access still excludes local, private, and link-local targets.
 */
export type NetworkPolicy = { "mode": "off" } | { "mode": "package_managers" } | { "mode": "allowed_hosts", allowed_hosts: Array<string>, package_managers: boolean, } | { "mode": "open" };

/**
 * Where opening the row takes the reader.
 */
export type NotificationContextSnapshot = { "surface": "chat", chat_id: ChatId, } | { "surface": "code", session_id: CodeSessionId, workspace_id: WorkspaceId, };

/**
 * Identifies one durable agent-finished notification.
 */
export type NotificationId = string;

export type NotificationKindSnapshot = "agent_completed" | "agent_failed";

export type NotificationPage = { notifications: Array<NotificationSnapshot>, next_cursor?: string, };

export type NotificationSnapshot = { id: NotificationId, kind: NotificationKindSnapshot, title: string, context: NotificationContextSnapshot, created_at: string, read_at?: string, };

export type NotificationUnreadCount = { unread: number, };

/**
 * Identifies one conversation-owned output across all of its revisions.
 *
 * This is the durable handle a model, renderer, or export names. It is
 * deliberately opaque: possession of an id is not authority, and it never
 * encodes a filename or a host path.
 */
export type OutputId = string;

/**
 * Whether connected-folder publication may replace an existing regular file.
 */
export type OutputWriteMode = "create" | "replace";

/**
 * Closed renderer-safe pending approval projection. Canonical arguments and
 * unknown tool names never cross this boundary; only a tool's own closed
 * preview of the action under review does. That preview may carry the call's
 * own `summary`, which the approval card does not render — consent is given to
 * an action, not to a sentence about one. See
 * `docs/decisions/0018-tool-call-narration.md`.
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
 * Renderer-safe write-back approval. Canonical output, root, and destination
 * identities remain native-only; the card can approve or decline this exact
 * call. The mode is carried so the card can name what is being decided —
 * creating a new file reads very differently from destroying an existing one.
 */
export type PendingOutputWritebackRequest = { call_id: CallId, turn_id: TurnId, mode: OutputWriteMode, claimed: boolean, };

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
 * How a conversation handles mutations and approvals.
 *
 * For the internal engine the mode governs the server-side approval gate,
 * which is where all but one mutating call lives. Client-executed tools run
 * in the trusted desktop under their own consent — a folder grant the reader
 * picked, a card the native side raises — and do not re-enter that gate. The
 * one client call that mutates something the reader owns, publishing an
 * output into a connected folder, consults the mode itself: see
 * [`crate::OutputWriteMode::requires_user_decision`].
 *
 * For an external agent engine each adapter maps these onto the engine's
 * native flags. A mode the engine cannot honor is refused at session
 * creation — never approximated.
 */
export type PermissionMode = "plan" | "ask" | "auto" | "allow";

/**
 * The two decisions a reader can make about a proposed plan.
 */
export type PlanDecisionChoice = "accept" | "reject";

/**
 * What a plugin can actually do, from a closed vocabulary.
 *
 * A badge is **derived by the host from the plugin's contents** and is never
 * read from a manifest: there is no `capabilities` key, and the parser's
 * closed key set rejects one outright, so a bundle cannot understate what it
 * carries or claim reach it does not have. This is the same honesty invariant
 * the model registry enforces on modality flags.
 *
 * Badges have two consumers. A UI shows them on a plugin's detail view, and
 * the permission layer keys install/enable confirmation on the heavier ones —
 * which is what keeps day-to-day skill invocation prompt-free.
 */
export type PluginCapability = "write-files" | "network" | "host-install" | "live-control" | "mcp";

/**
 * Everything this installation has, in the state it is in.
 */
export type PluginCatalog = { 
/**
 * Bundles in load order (by slug), each with its members.
 */
plugins: Array<PluginInfo>, 
/**
 * Skills no bundle claims — user-authored packages land here.
 */
skills: Array<PluginSkillInfo>, 
/**
 * Every installed prompt, bundled or standalone, in one flat list.
 *
 * Flat rather than nested under its bundle because the consumer is a
 * picker over the whole library; a plugin's members are the entries whose
 * `plugin` names it.
 */
prompts: Array<PluginPromptInfo>, };

/**
 * What kind of work a plugin bundles, from a closed vocabulary.
 *
 * Closed on purpose, like [`crate::HostDep`]: an unknown value rejects the
 * manifest instead of parsing into a string no grouping or badge can act on.
 */
export type PluginCategory = "documents" | "data" | "visualization" | "other";

/**
 * Renderer-safe compatibility disclosure for one plugin.
 */
export type PluginCompatibility = { status: PluginCompatibilityStatus, issues: Array<PluginCompatibilityIssue>, };

/**
 * One reason an imported plugin is not statically sandbox-compatible.
 */
export type PluginCompatibilityIssue = { "kind": "missing_sandbox_dependency", skill: string, dependency: string, } | { "kind": "scripts_present", skill: string, };

/**
 * The static sandbox-compatibility conclusion recorded at import time.
 */
export type PluginCompatibilityStatus = "compatible" | "limited" | "unchecked";

/**
 * Body of `PUT /plugins/enabled`. Absent names are left alone.
 */
export type PluginEnableUpdate = { 
/**
 * Bundle flags to set, by slug.
 */
plugins: { [key in string]: boolean }, 
/**
 * Skill flags to set, by slug. Setting one inside a disabled bundle is
 * allowed and remembered; it takes effect when the bundle comes back.
 */
skills: { [key in string]: boolean }, };

/**
 * One bundle, as a management surface renders it.
 */
export type PluginInfo = { 
/**
 * The slug the toggle route addresses it by.
 */
name: string, display_name: string, description: string, category: PluginCategory, 
/**
 * Where the bundle was loaded from; host-derived, never claimed.
 */
origin: PluginOrigin, 
/**
 * What the bundle can do, derived by the host from what it contains.
 * Never self-declared: a manifest has no key for this.
 */
capabilities: Array<PluginCapability>, 
/**
 * Import-time static compatibility disclosure. A hand-authored bundle is
 * explicitly unchecked; imported bundles say whether they fit the
 * prepared sandbox image and why not.
 */
compatibility: PluginCompatibility, 
/**
 * Whether the bundle is on. Off gates every member regardless of the
 * member's own flag, which the member entries still report unchanged.
 */
enabled: boolean, 
/**
 * Member skills in manifest order.
 */
skills: Array<PluginSkillInfo>, };

/**
 * Which source a validated plugin package was loaded from.
 *
 * Origin is host-derived from the load path, never from manifest content —
 * the closed key set has no `origin` key at all — so a user bundle cannot
 * claim to ship with the app. A management surface uses it to attribute the
 * bundles the user wrote themselves.
 */
export type PluginOrigin = "builtin" | "user";

/**
 * One reusable prompt, as a picker or a management surface renders it.
 *
 * Deliberately without a body: the text is fetched from
 * [`get_prompt_body`] when the user actually picks one, so the catalog stays
 * bytes per entry no matter how long the prompts are.
 */
export type PluginPromptInfo = { 
/**
 * The slug the body route addresses it by.
 */
name: string, 
/**
 * The tip a card or popover shows.
 */
description: string, 
/**
 * Where the package was loaded from; host-derived, never claimed.
 */
origin: PromptOrigin, 
/**
 * The bundle that claims this prompt, if any. `None` is a standalone
 * package — every user-authored prompt is one.
 */
plugin: string | null, 
/**
 * Whether the prompt is offered. A prompt has no flag of its own: a
 * bundled one follows its bundle, and a standalone one is always on.
 */
enabled: boolean, };

/**
 * One skill, inside a bundle or standing alone.
 */
export type PluginSkillInfo = { name: string, description: string, 
/**
 * Where the package was loaded from; host-derived, never claimed.
 */
origin: SkillOrigin, 
/**
 * The skill's *own* flag, independent of any owning bundle's — so a UI
 * can show the member choices that come back when a bundle is re-enabled.
 */
enabled: boolean, };

/**
 * An optional grouping of chats that share project context and a document
 * corpus. A chat may belong to a project or stand alone — unlike some designs
 * that make a project mandatory, Tidebreak keeps loose, projectless chats.
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
 * One prompt's insertable text, fetched when the user picks it.
 *
 * Its own route for the same reason skill instructions have one: the catalog
 * is fetched far more often than any one prompt is inserted.
 */
export type PromptBody = { name: string, 
/**
 * The `PROMPT.md` markdown below the frontmatter — exactly what goes into
 * the composer. It is never composed into the model's operating prompt;
 * it reaches a model only if the user sends the message.
 */
body: string, };

/**
 * Which source a validated prompt package was loaded from.
 *
 * Host-derived from the load path, never from manifest content, so a user
 * package cannot claim to be built-in.
 */
export type PromptOrigin = "builtin" | "user";

/**
 * How a provider's credential was established.
 */
export type ProviderAuthMode = "api_key" | "chatgpt";

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
 * Whether a credential is stored (never the credential itself).
 */
has_credential: boolean, 
/**
 * How OpenAI (or similarly dual-mode providers) is authenticated.
 */
auth_mode?: ProviderAuthMode, 
/**
 * Explicit configured model entries for this endpoint.
 */
models: Array<CustomModelConfig>, };

/**
 * The known provider kinds. `#[non_exhaustive]` so new kinds can land without
 * breaking wire clients that match on the string form.
 */
export type ProviderKind = "anthropic" | "openai" | "xai" | "gemini" | "fireworks" | "together" | "openrouter" | "ollama" | "openai_compatible" | "model_gateway";

/**
 * One CI check on a pull request.
 */
export type PullRequestCheck = { 
/**
 * Check name as the host reports it.
 */
name: string, 
/**
 * pass, pending, fail, or skipped.
 */
bucket: PullRequestCheckBucket, 
/**
 * Host status phrase, when distinct from the bucket.
 */
detail?: string, 
/**
 * Host URL for this check, when known.
 */
url?: string, };

/**
 * Coarse CI bucket used to color a check row.
 */
export type PullRequestCheckBucket = "pass" | "pending" | "fail" | "skipped";

/**
 * One pull-request comment: an issue comment, a review body, or an inline
 * review comment. Never persisted; fetched live from the host.
 */
export type PullRequestComment = { 
/**
 * Where on the PR the comment lives.
 */
kind: PullRequestCommentKind, 
/**
 * Stable host identifier, normalized to text across GraphQL and REST
 * comment shapes.
 */
id?: string, 
/**
 * Author login, when the host reported one.
 */
author?: string, 
/**
 * Author avatar URL, when the host reported one.
 */
avatar_url?: string, 
/**
 * Host page for the comment or review, when available.
 */
url?: string, 
/**
 * Host creation timestamp, verbatim.
 */
created_at?: string, 
/**
 * Comment body, markdown as the host stores it.
 */
body: string, 
/**
 * Lowercased review verdict (approved, changes_requested, commented), on
 * review bodies only.
 */
review_state?: string, 
/**
 * File path, on inline review comments.
 */
path?: string, 
/**
 * Line number, on inline review comments when the host reports one.
 */
line?: number, };

/**
 * Which surface of the PR a comment belongs to.
 */
export type PullRequestCommentKind = "issue" | "review" | "inline";

/**
 * Bounded pull-request digest stored on a workspace.
 */
export type PullRequestDigest = { 
/**
 * PR number on the host.
 */
number: number, 
/**
 * Host URL, when known.
 */
url?: string | null, 
/**
 * Host state token (open, merged, closed, …).
 */
state: string, 
/**
 * PR title, when the host reported one.
 */
title?: string, 
/**
 * One-line checks summary.
 */
checks_summary?: string | null, 
/**
 * Individual checks, when the host reported any.
 */
checks?: Array<PullRequestCheck>, 
/**
 * True when the host reports the PR as a draft.
 */
draft?: boolean, 
/**
 * True when the host reports the PR merged.
 */
merged?: boolean, 
/**
 * Lowercased host review decision (approved, changes_requested, review_required).
 */
review_decision?: string, 
/**
 * Lowercased host mergeability (mergeable, conflicting, unknown).
 */
mergeable?: string, 
/**
 * Lowercased host merge-state status (clean, blocked, behind, dirty, …).
 */
merge_state_status?: string, 
/**
 * Head branch name on the host.
 */
head_branch?: string, 
/**
 * Base branch name on the host.
 */
base_branch?: string, 
/**
 * Head commit SHA the digest was read against, when the host reported
 * one. The watch sweep uses it to avoid re-fixing the same head.
 */
head_sha?: string, 
/**
 * True when auto-merge is enabled on the host.
 */
auto_merge_enabled?: boolean, in_merge_queue?: boolean, };

/**
 * One durable queued follow-up: a message parked while the session or its
 * workspace checkout was busy, promoted FIFO once the session is free.
 *
 * `id` names the row for edits and retraction, and is the turn id the
 * promoted turn is inserted under. `position` is 0-based and dense within
 * the session.
 */
export type QueuedCodeTurn = { id: CodeTurnId, session_id: CodeSessionId, message: string, position: number, created_at: string, updated_at: string, };

/**
 * The id is the client-generated turn id promotion will accept under, so an
 * ambiguous promotion retry resolves to `Existing` rather than a duplicate
 * turn. Rows are FIFO by `position` within a chat and fully durable: a queue
 * survives restarts and is visible to every client on the chat.
 */
export type QueuedTurn = { 
/**
 * The turn id this row becomes when promoted.
 */
id: TurnId, 
/**
 * Owning chat.
 */
chat_id: ChatId, 
/**
 * Byte-exact user message.
 */
content: string, 
/**
 * Image-attachment ids, in display order.
 */
attachments: Array<string>, 
/**
 * Chat-owned document ids.
 */
file_attachments: Array<DocumentId>, 
/**
 * Skills the user explicitly invoked.
 */
invoked_skills: Array<string>, 
/**
 * Whether the message was dictated.
 */
voice_input_used: boolean, 
/**
 * FIFO order within the chat.
 */
position: number, 
/**
 * When the message was queued.
 */
created_at: string, 
/**
 * When it was last edited or reordered.
 */
updated_at: string, };

/**
 * A named command the user can run in a workspace.
 */
export type QuickAction = { 
/**
 * Display name.
 */
name: string, 
/**
 * Command to run in the worktree.
 */
command: string, 
/**
 * When true, run once after workspace creation.
 */
auto_run_on_create: boolean, };

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
export type ReasoningEffort = "none" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra";

export type RendererAgentEvent = { "type": "turn_started", turn_id: TurnId, } | { "type": "text_delta", text: string, } | { "type": "reasoning_delta", text: string, } | { "type": "stream_interrupted" } | { "type": "tool_call_started", call_id: CallId, name: RendererToolName, } | { "type": "tool_call_args_delta", call_id: CallId, } | { "type": "user_questions_asked", call_id: CallId, turn_id: TurnId, } | { "type": "plan_proposed", call_id: CallId, turn_id: TurnId, } | { "type": "task_plan_updated", call_id: CallId, turn_id: TurnId, } | { "type": "approval_required", call_id: CallId, action: RendererToolName, approval: ToolApprovalKind, class: ApprovalClass, 
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
 * A bounded, server-authored reason the renderer can act on without
 * receiving model-facing output or executor diagnostics.
 */
failure?: RendererToolFailure, 
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
result?: ToolResultPreview, } | { "type": "turn_completed", usage: RendererTurnUsage, } | { "type": "turn_refused", refusal: RendererRefusal, usage: RendererTurnUsage, } | { "type": "turn_failed", 
/**
 * Why the turn failed, at the only resolution a client can act on.
 * The internal `kind` stays behind the server; allowlisted provider
 * diagnostics may cross separately as `detail`.
 */
category: TurnFailureCategory, 
/**
 * Bounded provider diagnostic, when the failure originated upstream.
 */
detail?: string, model?: RendererModelIdentity, } | { "type": "turn_cancelled", usage: RendererTurnUsage, } | { "type": "user_steered", message_id: MessageId, text: string, } | { "type": "context_truncated", 
/**
 * Estimated transcript tokens before the reduction.
 */
original_tokens: number, 
/**
 * Estimated transcript tokens after fitting to the budget.
 */
fitted_tokens: number, } | { "type": "compaction_started" } | { "type": "compaction_finished", 
/**
 * Whether a new (or confirmed) checkpoint was stored.
 */
compacted: boolean, } | { "type": "event_omitted" };

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
export type RendererChatMetadata = { "metadata": "titled", title: string, } | { "metadata": "file_changes_recorded", turn_id: TurnId, } | { "metadata": "sandbox_preparing", preparing: boolean, };

/**
 * Exact model route involved in a provider failure, with no diagnostics.
 */
export type RendererModelIdentity = { id: string, provider: ProviderKind, };

/**
 * Bounded refusal metadata safe to present in the desktop transcript.
 */
export type RendererRefusal = { category: string | null, partial_output: boolean, };

export type RendererSequencedEvent = { seq: number, event: RendererAgentEvent, replayed?: boolean, };

export type RendererToolFailure = { code: RendererToolFailureCode, reason: RendererToolFailureReason, };

export type RendererToolFailureCode = "executor_unavailable";

export type RendererToolFailureReason = "lease_expired";

/**
 * A tool name the renderer is allowed to present.
 *
 * The desktop's union, its runtime guard, its copy table, and its icon table
 * are all generated from this enum, so a variant added here cannot leave one of
 * them behind — see `docs/wire-types.md`.
 */
export type RendererToolName = "search" | "list_documents" | "read_document" | "read_tool_result" | "web_search" | "web_extract" | "read_delegated_file" | "read_file" | "list_dir" | "write_file" | "request_folder_access" | "connect_folder" | "list_connected_folders" | "list_folder" | "read_connected_file" | "import_connected_file" | "write_output_to_connected_folder" | "spawn_sandbox_agent" | "wait_for_agents" | "ask_user_questions" | "exit_plan_mode" | "update_task_plan" | "exec" | "create_app" | "other";

export type RendererToolStatus = "completed" | "failed";

/**
 * A turn's token accounting, as the renderer needs it.
 *
 * The four counts are disjoint. Every adapter normalizes to the same split
 * before the count reaches the journal: `input_tokens` is the *fresh*,
 * uncached prompt only, and never includes `cache_read_input_tokens` or
 * `cache_creation_input_tokens`. Anthropic reports that split natively; the
 * OpenAI, OpenAI-compatible, and Gemini paths all subtract the cached portion
 * out of the provider's prompt total before filling `input_tokens`. So the
 * tokens that occupied the model's context for this turn are the plain sum of
 * all four fields — no term is double-counted and none is missing.
 *
 * One caveat a reader of these numbers has to hold: they are the turn's
 * totals, summed over every model call the agent made, not a snapshot of the
 * final prompt. A turn that ran ten tool calls re-sent its transcript ten
 * times, so the sum exceeds what was resident in the window at any one moment.
 * It is a faithful measure of what the turn cost and a ceiling on what the
 * window held.
 */
export type RendererTurnUsage = { 
/**
 * Fresh prompt tokens, excluding both cache figures below.
 */
input_tokens: number, 
/**
 * Tokens the model generated.
 */
output_tokens: number, 
/**
 * Prompt tokens served from the provider's cache.
 */
cache_read_input_tokens: number, 
/**
 * Prompt tokens written to the provider's cache.
 */
cache_creation_input_tokens: number, };

/**
 * Identifies a registered local git repository.
 */
export type RepoId = string;

/**
 * Non-authoritative, well-known starting location for the native picker.
 *
 * This is deliberately not a free-form path. The trusted desktop decides how
 * (or whether) to map it to a local picker location.
 */
export type RequestedFolderHint = "documents" | "downloads";

/**
 * Body for validating manually tracked GitHub repositories. Values may be
 * `owner/repo`, `host/owner/repo`, or a GitHub HTTPS/SSH URL.
 */
export type ResolveCodeDeliveryRepositoriesBody = { repositories: Array<string>, };

/**
 * Whether a `rest_api` record's referenced credential currently resolves.
 */
export type RestCredentialStatus = "none" | "configured" | "missing";

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
media_type: string | null, 
/**
 * The durable record this row names, when the renderer can open one.
 *
 * Data rather than display text: the renderer routes a click through its
 * own panel navigation and never prints the id, and [`ResultEntryKind`]
 * names where that click goes — an [`Output`] row opens the outputs
 * panel, an [`App`] row the apps library. A kind with nowhere to go
 * leaves this `None`.
 *
 * `default` because retained projections predate the field.
 *
 * [`Output`]: ResultEntryKind::Output
 * [`App`]: ResultEntryKind::App
 */
target_id: string | null, 
/**
 * The public page this row opens, when it names one.
 *
 * The one projected field a click leaves the application with, so it is
 * admitted at construction rather than at the click: only `http` and
 * `https` survive [`Self::with_web_url`], and a row whose address is
 * anything else keeps its title and its domain and simply does not open.
 * It rides alongside the domain in `detail` rather than replacing it —
 * a column of rows is still told apart by host, not by query string.
 *
 * Carried because attribution for a search a chat did not run itself is
 * only meaningful if the reader can reach the page. `default` because
 * retained projections predate the field.
 */
url?: string, };

/**
 * What one row of a listed result is, which is what picks its icon.
 *
 * A closed vocabulary rather than an icon name: the renderer chooses how to
 * draw a folder, and a tool must not be able to name a glyph.
 */
export type ResultEntryKind = "file" | "folder" | "source" | "passage" | "link" | "output" | "app";

/**
 * One thing a call could not do.
 *
 * A batch tool succeeds and fails in the same breath — five files import, two
 * do not — and a card that lists only what worked is not reporting, it is
 * flattering. So every local-file result carries a parallel failures list.
 *
 * Two fields because that is what a failure row reads as: the thing that
 * failed, and why.
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
 * One event on the per-session WebSocket.
 */
export type SequencedCodeEventFrame = { 
/**
 * Journal position. On a `transient` frame this is the cursor the event
 * streamed behind, not a position the frame occupies — resume from it
 * and you lose nothing, because no row holds this event.
 */
seq: number, event: CodeEvent, replayed?: boolean, 
/**
 * Set on a live-only event the journal does not hold: assistant deltas,
 * and the catch-up delta a mid-turn reader gets on connect. Apply it but
 * do not advance the resume cursor. A reconnect may receive the complete
 * current tail with `replacement` set (record 57).
 */
transient?: boolean, 
/**
 * Set on a transient assistant delta that contains the complete live
 * tail. Replace the current assistant buffer instead of appending it.
 */
replacement?: boolean, 
/**
 * Set on the first replayed frame of a capped window: older events above
 * the requested cursor were dropped, and the history in front of this
 * frame is not coming.
 */
truncated?: boolean, };

/**
 * Body of `PUT /code/worktree-root`. A null or blank root clears the setting.
 */
export type SetCodeWorktreeRootBody = { root: string | null, };

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
has_api_key: boolean, 
/**
 * The sticky new-chat defaults, so a composer for a chat that does not
 * exist yet can show what `POST /chats` will seed.
 */
chat_defaults: StickyChatDefaults, 
/**
 * Preferred maximum concurrent background agents. Spawn unsettled
 * children on one origin turn are further capped at
 * [`AgentRun::MAX_ACTIVE_BACKGROUND_AGENTS`] (wait_for_agents membership).
 */
max_active_background_agents: number, 
/**
 * Model steps a background agent takes before it must check in.
 *
 * A cadence, not a cap: reaching it never fails the run — the agent wraps
 * up with what it has and reports back for direction.
 */
sandbox_agent_checkin_steps: number, 
/**
 * Consecutive failed tool calls after which a background agent checks in.
 */
sandbox_agent_error_checkin: number, 
/**
 * When and how hard semantic compaction may run.
 */
compaction: CompactionSettings, 
/**
 * Per-model deviations from the catalog's `recommended` flag, keyed by the
 * same provider-qualified selection key `ModelInfo.key` and a chat's model
 * carry (`"<provider>::<id>"`).
 *
 * Deviations only: a model with no entry uses its catalog default, and
 * resetting one to the default means sending the map without that key.
 * `PUT /settings` **replaces this map wholesale** rather than merging, so
 * a writer sends the complete set of deviations it wants to persist.
 *
 * Visibility is a picker concern: the server stores and serves this map
 * and never filters `GET /models` by it. A hidden model remains fully
 * valid for existing chats, replay, and explicit selection.
 */
model_visibility_overrides: { [key in string]: ModelVisibility }, 
/**
 * Whether the computer-use capability (screen capture + app control) is
 * enabled. Read at boot; turning it off unregisters the tools on the next
 * launch.
 */
computer_use_enabled: boolean, 
/**
 * Whether completed code turns rewrite their closing message into lucid
 * prose. Default off.
 */
rewrite_closing_messages: boolean, };

/**
 * Renderer-safe progress of the current sign-in attempt.
 */
export type SignInProgress = { "state": "idle" } | { "state": "pending", authorization_url: string, } | { "state": "failed", message: string, };

/**
 * One skill's instruction body, for the management surface's detail view.
 *
 * Its own route rather than a catalog field on purpose: a body is kilobytes
 * where a catalog row is bytes, and the catalog is fetched far more often
 * than any one skill is read.
 */
export type SkillInstructions = { name: string, 
/**
 * The `SKILL.md` markdown body, with the frontmatter removed — what the
 * model is taught when the skill is staged, shown to the reader verbatim.
 */
instructions: string, };

/**
 * Which source a validated skill package was loaded from.
 *
 * Origin is host-derived from the load path, never from manifest content, so
 * a user package cannot claim to be built-in. The prompt catalog uses it to
 * attribute user-authored entries.
 */
export type SkillOrigin = "builtin" | "user";

/**
 * What a document declares, for the configuration form's operation picker.
 * Renderer-safe: ids, methods, paths, and truncated summaries only.
 */
export type SpecPreviewInfo = { 
/**
 * Hex SHA-256 of the raw document — the pin the upsert must carry back
 * with a URL source.
 */
document_sha256: string, operations: Array<SpecPreviewOperation>, 
/**
 * Operations the document declares that cannot be selected (no
 * well-formed `operationId`, an over-bound path, or a repeated id).
 */
unlistable: number, 
/**
 * Whether the operation list was cut at the inventory bound.
 */
truncated: boolean, };

export type SpecPreviewOperation = { operation_id: string, 
/**
 * Lowercase HTTP method, as a path item declares it.
 */
method: string, path: string, summary: string | null, };

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
 * The reader's last explicit per-chat choices — what an unspecified field of
 * `POST /chats` seeds. A `None` field has no recorded choice and keeps the
 * hard default (configured model, `ask`, open network).
 *
 * The permission mode is reported clamped to any managed ceiling, so what a
 * picker displays is what creation will actually seed.
 */
export type StickyChatDefaults = { model: string | null, reasoning_effort: ReasoningEffort | null, permission_mode: PermissionMode | null, network_policy: NetworkPolicy | null, };

/**
 * One file a background run submitted, as the renderer sees it.
 */
export type SubmittedOutputSnapshot = { output_id: OutputId, 
/**
 * The name the run gave the file, which is the output's name.
 */
filename: string, };

/**
 * Renderer-safe durable projection of a chat's current plan.
 */
export type TaskPlan = { 
/**
 * The turn whose call last replaced this plan.
 */
turn_id: TurnId, 
/**
 * The steps, in order.
 */
steps: Array<TaskPlanStep>, 
/**
 * When the last replacement committed.
 */
updated_at: string, };

/**
 * One step of the plan.
 */
export type TaskPlanStep = { 
/**
 * What this step does, as one short imperative line.
 */
content: string, 
/**
 * Where the step stands: `pending` before it starts, `in_progress` while
 * it is being worked on (at most one step at a time), `completed` after.
 */
status: TaskPlanStepStatus, };

/**
 * Where one step stands.
 */
export type TaskPlanStepStatus = "pending" | "in_progress" | "completed";

/**
 * The action a call will take, in a form a human can inspect.
 *
 * Approval cards need this because consent to an action you cannot see is not
 * consent. Result cards reuse it so the same action is described the same way
 * before and after it runs.
 *
 * Most variants also carry a `summary`: one sentence the model wrote about
 * what its own call is doing. It is **display-only**, and it is the one field
 * here that is prose rather than a projection of the action. See
 * `docs/decisions/0018-tool-call-narration.md` — it never reaches an approval
 * card, never reaches the auto-approval judge, and is never part of a grant's
 * identity ([`Self::without_summary`]), because a call that could describe
 * itself to a consent decision could describe itself favourably.
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
cwd: string, 
/**
 * Scratch-relative files and directories staged into the sandbox
 * before the command runs, empty when the model staged none.
 *
 * Part of the action, not incidental setup: this list is what the
 * command can read, so a person approving `python3 analyze.py` is
 * entitled to see which of their files it is being handed. Omitting
 * it also made two calls that stage different documents project
 * identically, which is what `describes_exactly` promises they do
 * not.
 *
 * `default` because grants and approval rows retained before the
 * field existed carry no staging list; they read back as staging
 * nothing, which is narrower than what they were given for and so
 * sends a call that stages anything back to the person.
 */
files: Array<string>, 
/**
 * Display-only narration; see the type's documentation.
 */
summary?: string, } | { "tool": "search", query: string, 
/**
 * Display-only narration; see the type's documentation.
 */
summary?: string, } | { "tool": "web_search", query: string, 
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
end_published_at: string | null, 
/**
 * Display-only narration; see the type's documentation.
 */
summary?: string, } | { "tool": "web_extract", url: string, 
/**
 * Display-only narration; see the type's documentation.
 */
summary?: string, } | { "tool": "write_file", 
/**
 * Workspace-relative destination path, never a host path.
 */
path: string, 
/**
 * Display-only narration; see the type's documentation.
 */
summary?: string, } | { "tool": "delegate_agent", 
/**
 * The child's self-contained task, as the model wrote it.
 */
task: string, 
/**
 * The network policy the child inherits from this chat.
 */
network: NetworkPolicy, };

/**
 * Closed immutable consent semantics stored with each approval request.
 *
 * Each presentable variant names the egress a human is consenting to, so the
 * renderer can describe the action without ever seeing the model-authored
 * arguments. `Unsupported` is the fail-closed default: a Sensitive
 * action the server can only reject, never approve.
 */
export type ToolApprovalKind = "search_may_share_query_and_excerpts" | "web_search_may_share_query" | "web_extract_may_fetch_url" | "exec_may_run_networked_command" | "external_mcp_may_call_server" | "workspace_may_modify_files" | "delegate_may_run_background_agent" | "computer_may_control_app" | "unsupported";

/**
 * Display-oriented classification of a tool the engine started.
 */
export type ToolDetail = { "kind": "command", 
/**
 * Command string.
 */
cmd: string, 
/**
 * Working directory, when reported.
 */
cwd: string, } | { "kind": "file_edit", 
/**
 * Path being edited.
 */
path: string, } | { "kind": "file_read", 
/**
 * Path being read.
 */
path: string, } | { "kind": "search", 
/**
 * Query string.
 */
query: string, } | { "kind": "other", 
/**
 * Bounded summary.
 */
summary: string, };

/**
 * How a tool call finished.
 */
export type ToolOutcome = "succeeded" | "failed" | "denied";

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
outputs?: Array<ResultEntry>, 
/**
 * How the execution backend fell short of its intended setup, when it
 * did. Reported on the first execution that degrades and not on the
 * ones after it, so a chat says this once rather than on every card.
 */
degraded?: ExecDegradation, 
/**
 * Which backend ran the command. Defaulted so exec rows persisted
 * before the field existed read back unchanged.
 */
backend?: ExecBackend, } | { "tool": "web_search_provider_required" } | { "tool": "mcp_app", 
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
elided: number, } | { "tool": "user_questions", answers: Array<AnsweredUserQuestion>, 
/**
 * Whatever the reader added on their own, when they added any.
 */
additional_context?: string, } | { "tool": "plan_decision", title: string, plan: string, 
/**
 * Whether the reader approved the plan as proposed.
 */
accepted: boolean, 
/**
 * What the reader asked to change, when they sent it back.
 */
feedback?: string, } | { "tool": "screen_capture", 
/**
 * The captured screenshot, content-addressed in the blob store.
 */
image: ImageRef, 
/**
 * How many interactive elements were marked on the capture, so the
 * card can say "capture with N controls" without the model's text.
 */
mark_count: number, };

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
 * the model's plumbing, so `System` and `Tool` never appear from storage as
 * tool rows — and that was previously guaranteed only by a `matches!` filter
 * at the one call site, while the snapshot's own type still admitted all four.
 * The renderer mirrored the narrow version and branched on `assistant` with no
 * third arm, so a `system` entry reaching it would have rendered as a user
 * message.
 *
 * Encoding it here makes the guarantee the type's rather than the caller's, and
 * makes a new [`Role`] variant a decision in [`Self::for_transcript`] instead of
 * something that silently appears in the transcript.
 *
 * [`Self::Compaction`] is synthetic: injected from the current context
 * checkpoint, never stored as a [`StoredMessage`].
 */
export type TranscriptRole = "user" | "assistant" | "system" | "compaction";

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
export type TurnFailureCategory = "rate_limited" | "auth" | "provider_access" | "transient" | "unknown";

/**
 * Identifies one turn: a single user input through to the final answer.
 */
export type TurnId = string;

/**
 * Switch a trigger on or off. The row survives either way, so the scoping
 * does not have to be rebuilt to turn a rule back on.
 */
export type UpdateCodeTriggerBody = { enabled: boolean, };

/**
 * One bounded question shown to the user.
 */
export type UserQuestion = { id: string, header: string, question: string, options: Array<UserQuestionOption>, question_type: UserQuestionType, allow_free_form: boolean, };

/**
 * One selectable answer choice.
 */
export type UserQuestionOption = { id: string, label: string, description: string, };

/**
 * Whether the reader may select one option or several independent options.
 */
export type UserQuestionType = "single_select" | "multi_select";

/**
 * How thoroughly Tidebreak has exercised a model's agent-facing behavior.
 */
export type VerificationTier = "verified" | "unverified";

/**
 * Public state returned by the local API. It intentionally reports only
 * selection, credential presence, and the configured instance URL — key
 * material never crosses the secret boundary.
 */
export type WebSearchConfigInfo = { provider?: WebSearchProviderKind, 
/**
 * Which search a turn gets. Orthogonal to the fields below, which report
 * only the host provider's readiness: a vendor turn is unaffected by all
 * of them.
 */
mode: WebSearchMode, timeout_ms: number, 
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
 * Which search a turn should use, as the operator chose it.
 *
 * The choice is about *who runs the search*, not about which engine: a vendor
 * search runs inside the model provider's own infrastructure and never touches
 * this host's providers, credentials, or egress policy. `Automatic` is the
 * default and the value every configuration written before this existed reads
 * back as, so an installation that had web search working keeps exactly the
 * search it had.
 */
export type WebSearchMode = "automatic" | "vendor" | "host" | "off";

/**
 * A configured web-search backend. The stable string also selects its secret
 * reference; it is intentionally not a model-controlled argument.
 */
export type WebSearchProviderKind = "exa" | "tavily" | "brave" | "searxng" | "model_provider";

/**
 * Identifies one isolated workspace (worktree + branch) on a repo.
 */
export type WorkspaceId = string;

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
  "list_documents",
  "read_document",
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
  "update_task_plan",
  "exec",
  "create_app",
  "other",
] as const;
