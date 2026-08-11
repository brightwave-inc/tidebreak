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
 * A bounded headline for one background-agent activity step.
 *
 * This is deliberately smaller than [`ToolActionPreview`]. The command,
 * arguments, and query are model-authored text: they are bounded for safe
 * single-line presentation, but may repeat any information the background
 * agent already saw and are therefore outside the host-field non-disclosure
 * guarantee. The projection copies no stored result text except a settled
 * `exec` receipt's bounded tail — see [`Self::with_exec_result`] — and never
 * copies host-only broker identity directly. Its only other host-derived
 * values are a typed numeric exit code and the leaf name of the canonically
 * admitted delegated file.
 *
 * Command and search fields use the foreground approval-card projection so
 * both surfaces share the same sanitization.
 */
export type AgentActivityDetail = { "kind": "exec", command: string, args: Array<string>, exit_code?: number, output?: string, } | { "kind": "search", query: string, } | { "kind": "file", name: string, };

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
export type AgentRunSnapshot = { id: AgentRunId, parent_id: AgentRunId | null, tier: AgentRunTier, execution_location: AgentRunExecutionLocation, status: AgentRunStatus, 
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
 * [`openwave_core::MAX_TASK_PLAN_STEP_CHARS`], or carrying any character the
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
 * One current-manifest binding, projected for the consent sheet.
 *
 * Exactly one of `app` and `folder` is present for the two locally resolved
 * vocabularies, matching what the binding names. An app-keyed row carries
 * `operation_ids`; a folder row carries `access`. A gateway binding carries
 * its operation ids and, once one reads back, the gateway app's display
 * name — it names neither a local record nor a root, so both id fields stay
 * absent until this projection carries the gateway's own id. The sheet
 * derives the combined-consent exfiltration warning
 * (docs/folder-bindings.md) from the rows themselves: a manifest with both a
 * folder row and a network row can read files and reach the network.
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
 * The access level a folder binding requests.
 */
access: FolderAccess | null, 
/**
 * The bound connected app's or folder's display name, absent when
 * nothing configured or approved answers to the id — the sheet says so
 * instead of showing a raw id alone.
 */
name: string | null, 
/**
 * Catalog `operationId`s the current manifest pins under this app, for a
 * `rest_api` binding.
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
export type AppInvokeRefusalKind = "app_not_found" | "not_pinned" | "consent_required" | "unknown_tool";

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
export type ChatTerminalTurnSnapshot = { turn_id: TurnId, message_id?: MessageId, status: ChatTerminalTurnStatus, partial_content: string, reasoning?: string, refusal?: RendererRefusal, failure_category?: TurnFailureCategory, failure_model?: RendererModelIdentity, file_changes: Array<ExecFileChangeSummary>, 
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
 * A small, human-scale position inside a cited document.
 *
 * Validation is intentionally loose. A page or line that does not exist still
 * renders and opens the document as close to that position as the reader can.
 */
export type CitationLocator = { "kind": "document" } | { "kind": "page", page: number, } | { "kind": "pages", start: number, end: number, } | { "kind": "lines", start: number, end: number, } | { "kind": "sheet", sheet: string, cells: string | null, };

/**
 * Renderer-safe configuration and readiness.
 */
export type CodeExecutionConfigInfo = { provider?: CodeExecutionProviderKind, timeout_ms: number, available: boolean, 
/**
 * Why the *selected* provider cannot run, when it cannot. Absent while
 * execution is available or no provider is selected at all.
 */
unavailable_reason?: CodeExecutionUnavailableReason, has_credential: boolean, 
/**
 * One row per shipped provider: whether it could run here at all, and the
 * reason it could not. This is what makes an unusable host legible —
 * "paste an E2B key" is visible instead of being inferred from a generic
 * execution failure.
 */
providers: Array<CodeExecutionProviderAvailability>, 
/**
 * The configured egress policy and each managed provider's enforcement
 * status, so the renderer can present the policy and disclose which
 * providers actually restrict egress today.
 */
egress: CodeExecutionEgressInfo, 
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
 * Structured capability report for one execution provider on this host.
 *
 * `available` and `unavailable_reason` are two views of one decision, made in
 * [`provider_availability`], so no surface has to re-derive whether a platform
 * supports a provider or whether a key is saved.
 */
export type CodeExecutionProviderAvailability = { provider: CodeExecutionProviderKind, available: boolean, unavailable_reason?: CodeExecutionUnavailableReason, };

/**
 * A configured code-execution backend.
 */
export type CodeExecutionProviderKind = "local" | "e2b" | "daytona" | "docker";

/**
 * Why a provider cannot execute anything on this host right now.
 *
 * A stable machine-readable code, not a sentence: the reason is decided where
 * the fact is known (the platform probe, the credential slot) and every
 * surface renders its own copy from the code. Reasons are what the user can
 * act on — install a key, switch provider — never an internal failure detail.
 */
export type CodeExecutionUnavailableReason = "unsupported_platform" | "missing_sandbox_binary" | "missing_credential" | "missing_container_runtime" | "container_runtime_unreachable";

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
 * The curated-list entry this server matched, when OpenWave has
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
export type ConsentMethodSnapshot = "approval_card" | "folder_picker" | "permission_dialog" | "operator_config" | "carried_forward";

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
 * Context limit used by OpenWave's reducer.
 */
context_window: number, 
/**
 * Maximum output sent to the endpoint.
 */
max_output_tokens: number, 
/**
 * Inputs OpenWave may place on this model's request.
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
export type DetachedAdmissionProviderInfo = { provider: CodeExecutionProviderKind, 
/**
 * Whether the gate would admit a detached run hosted by this provider.
 */
admitted: boolean, 
/**
 * Every unmet precondition, named — not just the first.
 */
denials: Array<DetachedAdmissionDenialReason>, };

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
 * A way the execution backend ran with less than its intended setup.
 *
 * Closed, and deliberately coarse: what a reader needs is what happened and
 * what it costs them, not which vendor API returned which status. A provider
 * that degrades a second way earns a second variant here rather than a free
 * string, because the sentence the card shows is written on this side.
 */
export type ExecDegradation = "sandbox_image_unavailable";

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
export type GrantScope = { "scope": "exact_action" } & ToolActionPreview | { "scope": "any_args_for", command: string, } | { "scope": "command_prefix", tokens: Array<string>, } | { "scope": "path_subtree", prefix: string, } | { "scope": "whole_tool" };

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
 * Image formats OpenWave will send to a provider.
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
 * The conversation to open. With `call_id`, the deep link back to the
 * exact transcript position the item paused at.
 */
chat_id: ChatId, 
/**
 * Absent, not null, while the conversation is still untitled.
 */
chat_title?: string, turn_id: TurnId, call_id: CallId, kind: InboxItemKind, 
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
 * The curated-list entry this definition matches, when OpenWave has
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
 * How thoroughly OpenWave has exercised this provider/model combination.
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
 *
 * The declaration order is ascending autonomy, and the derived `Ord` relies
 * on it: `Plan < Ask < Auto < Allow`, matching [`Self::ALL`]. Managed-policy
 * ceilings compare modes with it, so a new variant must slot into this
 * scale, not just onto the end of the list.
 *
 * The mode governs the server-side approval gate, which is where all but one
 * mutating call lives. Client-executed tools run in the trusted desktop under
 * their own consent — a folder grant the reader picked, a card the native side
 * raises — and do not re-enter that gate. The one client call that mutates
 * something the reader owns, publishing an output into a connected folder,
 * consults the mode itself: see
 * [`crate::OutputWriteMode::requires_user_decision`].
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
export type ProviderKind = "anthropic" | "openai" | "xai" | "gemini" | "fireworks" | "together" | "openai_compatible" | "model_gateway";

/**
 * One message accepted while its chat had a live turn, waiting its turn.
 *
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
 * The failure's `kind` and `message` stay internal.
 */
category: TurnFailureCategory, model?: RendererModelIdentity, } | { "type": "turn_cancelled", usage: RendererTurnUsage, } | { "type": "user_steered", message_id: MessageId, text: string, } | { "type": "context_truncated", 
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
 * Non-authoritative, well-known starting location for the native picker.
 *
 * This is deliberately not a free-form path. The trusted desktop decides how
 * (or whether) to map it to a local picker location.
 */
export type RequestedFolderHint = "documents" | "downloads";

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
 * Maximum nonterminal spawned agents allowed in one chat.
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
model_visibility_overrides: { [key in string]: ModelVisibility }, };

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
files: Array<string>, } | { "tool": "search", query: string, } | { "tool": "web_search", query: string, 
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
end_published_at: string | null, } | { "tool": "web_extract", url: string, } | { "tool": "write_file", 
/**
 * Workspace-relative destination path, never a host path.
 */
path: string, } | { "tool": "delegate_agent", 
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
export type ToolApprovalKind = "search_may_share_query_and_excerpts" | "web_search_may_share_query" | "web_extract_may_fetch_url" | "exec_may_run_networked_command" | "external_mcp_may_call_server" | "workspace_may_modify_files" | "delegate_may_run_background_agent" | "unsupported";

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
feedback?: string, };

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
export type TurnFailureCategory = "rate_limited" | "auth" | "transient" | "unknown";

/**
 * Identifies one turn: a single user input through to the final answer.
 */
export type TurnId = string;

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
 * How thoroughly OpenWave has exercised a model's agent-facing behavior.
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
