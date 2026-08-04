import {
  RENDERER_TOOL_NAMES,
  type AppDetail as WireAppDetail,
  type AppGrantState as WireAppGrantState,
  type AppInvokeRefusalKind,
  type AppLibrary as WireAppLibrary,
  type AppSummary as WireAppSummary,
  type AppViewSession,
  type ApprovalGrantRung,
  type ApprovalClass,
  type AssistantCitationSnapshot,
  type CitationLocator as WireCitationLocator,
  type ChatMessageSnapshot,
  type PendingApprovalSnapshot,
  type AgentActivitySnapshot,
  type AgentActivityHistoryItem,
  type AgentActivityKind,
  type AgentActivityOutcome,
  type AgentRunCancellationSnapshot,
  type AgentRunSnapshot,
  type Chat as WireChat,
  type ChatTranscript as WireChatTranscript,
  type CodeExecutionConfigInfo as WireCodeExecutionConfigInfo,
  type ConnectedAppInfo as WireConnectedAppInfo,
  type ConnectedAppsInfo as WireConnectedAppsInfo,
  type CredentialPlacement as WireCredentialPlacement,
  type RestCredentialStatus as WireRestCredentialStatus,
  type SpecPreviewInfo as WireSpecPreviewInfo,
  type SpecPreviewOperation as WireSpecPreviewOperation,
  type CodeExecutionCredentialReadiness as WireCodeExecutionCredentialReadiness,
  type CodeExecutionProviderKind as WireCodeExecutionProviderKind,
  type EgressConfig as WireEgressConfig,
  type CustomModelConfig as WireCustomModelConfig,
  type McpHealth as WireMcpHealth,
  type McpServerDefinition as WireMcpServerDefinition,
  type McpViewSession,
  type GatewayApps as WireGatewayApps,
  type GatewayAppInfo as WireGatewayAppInfo,
  type GatewayStatus as WireGatewayStatus,
  type SignInProgress,
  type StandingGrantSnapshot,
  type ConsentStatementSnapshot,
  type GrantLevel,
  type GrantScope,
  type InboxItemKind,
  type McpServerInfo as WireMcpServerInfo,
  type ManagedPolicy as WireManagedPolicy,
  type ManagedPolicySource as WireManagedPolicySource,
  type McpServersInfo as WireMcpServersInfo,
  type ModelInfo as WireModelInfo,
  type ModelRole as WireModelRole,
  type ModelRoleInfo as WireModelRoleInfo,
  type Project as WireProject,
  type ProviderInfo as WireProviderInfo,
  type ProviderKind as WireProviderKind,
  type PermissionMode as WirePermissionMode,
  type NetworkPolicy as WireNetworkPolicy,
  type ReasoningEffort as WireReasoningEffort,
  type Settings,
  type StickyChatDefaults as WireStickyChatDefaults,
  type WebSearchConfigInfo as WireWebSearchConfigInfo,
  type WebSearchCredentialReadiness as WireWebSearchCredentialReadiness,
  type WebSearchProviderKind as WireWebSearchProviderKind,
  type PendingFolderAccessRequest as WirePendingFolderAccessRequest,
  type PendingOutputWritebackRequest as WirePendingOutputWritebackRequest,
  type PendingPlanApproval as WirePendingPlanApproval,
  type PendingUserQuestions as WirePendingUserQuestions,
  type UserQuestion as WireUserQuestion,
  type UserQuestionOption as WireUserQuestionOption,
  type UserQuestionType as WireUserQuestionType,
  type ChatTerminalTurnSnapshot,
  type ChatToolActivitySnapshot,
  type ChatToolActivityStatus,
  type ExecBackend,
  type ExecDegradation,
  type ExecFileChangeSummary as WireExecFileChangeSummary,
  type RendererAgentEvent,
  type RendererChatFrame,
  type RendererChatMetadata,
  type RendererRefusal,
  type RendererSequencedEvent,
  type RendererToolName,
  type ResultEntryKind,
  type ToolActionPreview,
  type TranscriptImageAttachment as WireTranscriptImageAttachment,
  type TranscriptRole,
  type ToolApprovalKind,
  type ToolResultPreview as WireToolResultPreview,
} from "./generated/wire";

export type {
  ApprovalClass,
  ApprovalGrantRung,
  InboxItemKind,
  GrantLevel,
  GrantScope,
  StandingGrantSnapshot,
  ConsentStatementSnapshot,
  ChatToolActivityStatus,
  TranscriptRole,
  RendererToolName,
  ResultEntryKind,
  ToolActionPreview,
  RendererRefusal,
  ExecBackend,
  ExecDegradation,
};

/**
 * The WebSocket frame and the events it carries, generated from the server's
 * renderer projection.
 *
 * Named for the renderer on the Rust side; the shorter names are what the app
 * has always used, so they are aliased rather than renamed everywhere.
 */
export type SequencedEvent = RendererSequencedEvent;
export type AgentEvent = RendererAgentEvent;

/**
 * Anything the chat socket can deliver: a journaled event at its sequence, or
 * chat metadata that changed outside the journal.
 */
export type ChatFrame = RendererChatFrame;

/** A metadata frame — today only a title the server derived for this chat. */
export type ChatMetadataFrame = RendererChatMetadata;

/** Generated from `ToolApprovalKind`. */
export type RendererApprovalKind = ToolApprovalKind;

/**
 * One source as the renderer may see it, paired with `ChatDocumentDetail` on the
 * server. Deliberately narrower than the catalog summary, which also carries the
 * source's `uri` and index bookkeeping — neither of which the renderer is given,
 * because a `uri` can name a place on the reader's disk.
 */
export type DocumentDetail = {
  document_id: string;
  media_type: string;
  title: string | null;
  readable: boolean;
  /**
   * Whether the source kept the bytes it was made from. A source with none —
   * a fetched web page, whose markup is not retained — has no original to draw,
   * so the panel offers only the extracted text rather than a tab that can
   * only fail.
   */
  has_original_bytes: boolean;
  updated_at: string;
  content: string;
};

/** Connection details from the Tauri host (`server_info` command). */
export type ServerInfo = {
  baseUrl: string;
  token: string;
};

export type ProviderKind = WireProviderKind;
/** Stable provider-scoped key used for new settings and chat overrides. */
export type ModelSelectionKey = `${ProviderKind}::${string}`;

/** How hard a reasoning-capable model should think before answering. */
export type ReasoningEffort = WireReasoningEffort;

/** How much a chat lets the agent do between approvals. */
export type PermissionMode = WirePermissionMode;

/** Network access granted to code execution in one conversation workspace. */
export type NetworkPolicy = WireNetworkPolicy;

/** The sticky new-chat defaults an unspecified `POST /chats` field seeds. */
export type StickyChatDefaults = WireStickyChatDefaults;

export type ProviderInfo = WireProviderInfo;

export type VoiceTranscriptionModel = "local" | "gpt4o_transcribe" | "gemini_flash";
export type LocalVoiceState =
  | "not_installed"
  | "downloading"
  | "ready"
  | "failed"
  | "unavailable";
export type LocalVoiceInfo = {
  state: LocalVoiceState;
  downloaded_bytes: number | null;
  total_bytes: number | null;
  error: string | null;
};
/** One entry of the local speech catalog, with its install state on this device. */
export type LocalVoiceModelInfo = {
  id: string;
  label: string;
  description: string;
  total_bytes: number;
  english_only: boolean;
  recommended: boolean;
  state: LocalVoiceState;
  downloaded_bytes: number | null;
  error: string | null;
};
export type VoiceTranscriptionInfo = {
  model: VoiceTranscriptionModel;
  local_model: string;
  local_models: LocalVoiceModelInfo[];
  openai_ready: boolean;
  gemini_ready: boolean;
};

export type CustomModelConfig = WireCustomModelConfig;

/**
 * A model the picker may offer.
 *
 * `key` is re-branded rather than taken from the wire: the server sends a plain
 * string, and `ModelSelectionKey` is the refinement the app relies on to keep a
 * provider-qualified key distinct from a bare model id. Written out with `Omit`
 * so the one divergence from the generated type is visible.
 */
export type ModelInfo = Omit<WireModelInfo, "key"> & {
  key: ModelSelectionKey;
};

export type ModelRole = WireModelRole;

/** A named model role, its pinned selection, and what it resolves to now. */
export type ModelRoleInfo = WireModelRoleInfo;

/** The catalog plus what each role resolves to (`GET /models`). */
export type ModelCatalog = {
  models: ModelInfo[];
  roles: ModelRoleInfo[];
};

/** Global runtime settings (`GET/PUT /settings`). */
/** Global runtime settings (`GET/PUT /settings`). */
export type RuntimeSettings = Settings;

export type Project = WireProject;

/** The fixed, host-owned search providers supported by this build. */
export type WebSearchProviderKind = WireWebSearchProviderKind;

/** Non-secret web-search policy and readiness for its selected provider. */
export type WebSearchConfigInfo = WireWebSearchConfigInfo;

/** Readiness only: the API never returns an existing provider key. */
export type WebSearchCredentialReadiness = WireWebSearchCredentialReadiness;

/** The fixed, host-owned code-execution providers supported by this build. */
export type CodeExecutionProviderKind = WireCodeExecutionProviderKind;

/** Non-secret code-execution selection, timeout policy, and host readiness. */
export type CodeExecutionConfigInfo = WireCodeExecutionConfigInfo;

/** Readiness only: the API never returns a saved managed-provider key. */
export type CodeExecutionCredentialReadiness =
  WireCodeExecutionCredentialReadiness;

/**
 * Host-owned egress policy for the managed sandboxes. Never a secret: only
 * domain patterns and CIDR blocks, or `open` for today's unrestricted egress.
 */
export type EgressConfig = WireEgressConfig;

export type McpHealth = WireMcpHealth;

/** Typed stdio process data. Values are argv entries, never shell source. */
export type McpServerDefinition = WireMcpServerDefinition;

/**
 * Renderer-safe health projection. No environment value of any kind is sent:
 * `env` and `env_from` carry names, and the values behind them live in the
 * host process or the OS credential store.
 */
export type McpServerInfo = WireMcpServerInfo;

export type McpServersInfo = WireMcpServersInfo;

/** The Connected apps listing: per-kind projections, both kinds. */
export type ConnectedAppsInfo = WireConnectedAppsInfo;
export type ConnectedAppInfo = WireConnectedAppInfo;
export type RestCredentialStatus = WireRestCredentialStatus;
export type SpecPreviewInfo = WireSpecPreviewInfo;
export type SpecPreviewOperation = WireSpecPreviewOperation;
/** Where a stored REST credential is injected: `"bearer"` or a named header. */
export type CredentialPlacement = WireCredentialPlacement;

/**
 * What a REST connected-app upsert does about the credential: clear it, keep
 * the stored one unchanged, or store a new value. The value travels only in
 * this request body and is never read back by any route.
 */
export type RestCredentialUpdate =
  | "none"
  | "keep"
  | { set: { value: string; placement: CredentialPlacement } };

/** The resolved managed-mode policy; read-only for the renderer. */
export type ManagedPolicy = WireManagedPolicy;
export type ManagedPolicySource = WireManagedPolicySource;

export type Chat = WireChat;

/**
 * One visible, durable transcript entry in conversation order.
 *
 * Generated apart from `citations`, which is deliberately wider than the wire:
 * the server always serializes it, as `[]` when empty. It is optional here
 * because the transcript is not run through a validator — it arrives as a parsed
 * cast — so the `?` is what makes the compiler demand a guard at the one place
 * that reads it. Narrowing it to match the wire would delete that guard rather
 * than earn it, so the override is spelled out instead of hidden.
 */
export type ChatMessage = Omit<
  ChatMessageSnapshot,
  "citations" | "image_attachments"
> & {
  citations?: ChatMessageCitation[];
  /** Omitted by the server when this message has no images. */
  image_attachments?: ChatMessageImageAttachment[];
};

/**
 * A bounded, renderer-safe evidence snapshot owned by one assistant message.
 *
 * Ownership is positional: the server nests each citation under its message and
 * deliberately skips `message_id` on the wire. Do not reintroduce it.
 */
export type ChatMessageCitation = AssistantCitationSnapshot;
export type CitationLocator = WireCitationLocator;

/** One durable image identity in a historical user message. */
export type ChatMessageImageAttachment = WireTranscriptImageAttachment;

/**
 * A fixed, terminal tool-card projection with no canonical tool data.
 *
 * `tool` is the allowlisted renderer name rather than display copy: the renderer
 * derives a live call's wording from its name, and carrying prose here meant a
 * second copy of it plus an inverse lookup, where a change on either side
 * silently broke hydration.
 */
export type ChatToolActivity = ChatToolActivitySnapshot;

/** One terminal turn's status and renderer-safe streamed presentation. */
export type ChatTerminalTurn = ChatTerminalTurnSnapshot;
export type ExecFileChangeSummary = WireExecFileChangeSummary;

export type ExecFileUndoStatus =
  | "restored"
  | "deleted"
  | "already_undone"
  | "stale"
  | "not_available"
  | "snapshot_missing"
  | "unavailable";

export type ExecFileUndoOutcome = {
  snapshot_id: string;
  folder_name: string;
  relative_path: string;
  status: ExecFileUndoStatus;
};

export type ChatTranscript = WireChatTranscript;

/** A durable foreground coordinator or sandboxed background run. */
/** A durable foreground coordinator or sandboxed background run. */
export type AgentRun = AgentRunSnapshot;

/** The complete renderer projection returned by one sandbox stop request. */
/** The complete renderer projection returned by one sandbox stop request. */
export type SandboxAgentCancellation = AgentRunCancellationSnapshot;

/**
 * Live agent activity is intentionally a closed vocabulary. The server never
 * sends tool inputs, results, host paths, grants, executor identities, leases,
 * provider identities, or diagnostics.
 */
/**
 * Live agent activity is intentionally a closed vocabulary. The server never
 * sends tool inputs, results, host paths, grants, executor identities, leases,
 * provider identities, or diagnostics.
 */
export type AgentActivity = AgentActivitySnapshot;

/**
 * One settled or live step in a background run's ordered activity history.
 *
 * Same closed vocabulary and posture as {@link AgentActivity}: the server
 * sends only a fixed kind, a coarse outcome, and a timestamp — never tool
 * inputs, queries, results, identities, paths, leases, or diagnostics.
 */
export type AgentActivityHistoryEntry = AgentActivityHistoryItem;



/**
 * What a call produced.
 *
 * The other exception. A command's output is the whole reason to run it;
 * withholding it leaves the transcript asserting that something happened
 * without ever showing what.
 *
 * Hand-written and camelCase, unlike its snake_case wire form. The Rust type is
 * carried in the journal, so renaming its fields would stop existing chats from
 * loading — the remap in `parseToolResultPreview` is the cheaper side to keep.
 * That remap reads the generated wire type, so a field renamed in Rust breaks
 * there at compile time rather than silently producing `undefined`.
 */
export type ToolResultPreview =
  | {
      tool: "exec";
      exitCode: number | null;
      timedOut: boolean;
      outputTruncated: boolean;
      stdout: string;
      stderr: string;
      images?: {
        attachmentId: string;
        mediaType: string;
        width: number;
        height: number;
      }[];
      /** Durable outputs the command's output/ files created or updated. */
      outputs?: ResultEntry[];
      /**
       * How the execution backend fell short of its intended setup, when it
       * did. Sent on the first command that degrades and not on the ones
       * after it, so a conversation says this once.
       */
      degraded?: ExecDegradation;
      /** Which backend ran the command, when the server said. */
      backend?: ExecBackend;
    }
  | {
      /** Web search is available after the reader configures a provider. */
      tool: "web_search_provider_required";
    }
  | {
      /** A reference to an MCP Apps view — never markup. The document is
       * fetched separately and rendered only inside the sandboxed frame. */
      tool: "mcp_app";
      server: string;
      resourceUri: string;
    }
  | {
      /** What a call found, read, or wrote, as the list of things it was. */
      tool: "entries";
      entries: ResultEntry[];
      /** What the same call could not do. */
      failures: ResultFailure[];
      /** Rows the server bounded away, counted rather than shown. */
      elided: number;
    };

/** One row of a listed result. */
export type ResultEntry = {
  kind: ResultEntryKind;
  label: string;
  detail: string | null;
  meta: string | null;
  /** The document's media type, when the row is a document with one. */
  mediaType: string | null;
  /** The durable output this row names, when the row is one. */
  outputId: string | null;
};

/** One thing a listed call could not do. */
export type ResultFailure = {
  /** What failed, when the tool could name it. */
  label: string | null;
  /** Why, in the tool's own words. */
  error: string;
};

/** The entries-shaped result the list card renders. */
export type EntriesResultPreview = Extract<ToolResultPreview, { tool: "entries" }>;

/** The exec-shaped result the command card renders. */
export type ExecResultPreview = Extract<ToolResultPreview, { tool: "exec" }>;

/** A single-use frame address for one MCP Apps view. */
export type McpViewSessionInfo = McpViewSession;

/** Progress of the one in-flight gateway browser sign-in. */
export type GatewaySignInProgress = SignInProgress;

/** Renderer-safe model-gateway connection state; never token material. */
export type GatewayStatus = WireGatewayStatus;

/** The signed-in user's entitled connected apps, fetched live per request. */
export type GatewayApps = WireGatewayApps;

/** One entitled connected app and the MCP endpoint slugs that carry it. */
export type GatewayAppInfo = WireGatewayAppInfo;

/**
 * Opaque envelope for a sandboxed MCP App view; never rendered directly.
 *
 * Hand-written on purpose: the payload's JSON fields are untyped passthrough
 * for the view, which the wire-type generator's precision guard refuses.
 */
export type McpAppPayload = {
  arguments?: unknown;
  content: string;
  structured_content?: unknown;
  is_error: boolean;
};

/** The Apps library listing: every live local app, newest activity first. */
export type AppLibrary = WireAppLibrary;

/** One Apps library row. */
export type AppSummary = WireAppSummary;

/** One app's detail: summary fields plus its revision history. */
export type AppDetail = WireAppDetail;

/** Renderer-safe grant state for one app: the consent sheet's whole input. */
export type AppGrantState = WireAppGrantState;

/** A single-use frame address for one stored app revision. */
export type AppViewSessionInfo = AppViewSession;

/**
 * Result of a granted app invoke, forwarded verbatim to the sandboxed frame.
 *
 * Hand-written on purpose, like [`McpAppPayload`] and for the same reason:
 * `structured_content` is opaque passthrough the renderer never interprets,
 * which the wire-type generator's precision guard refuses.
 */
export type AppInvokeResult = {
  content: string;
  structured_content?: unknown;
  is_error: boolean;
};

/**
 * Result of a granted REST-operation invoke — {@link AppInvokeResult}'s
 * sibling for `rest_api` bindings, hand-written for the same reason.
 *
 * An executed operation is opaque passthrough: whatever HTTP status the API
 * answered (4xx/5xx included) with `is_error: false` and the raw response
 * body base64-encoded in `body_base64` so binary responses survive JSON. A
 * refused or failed execution is `is_error: true` with the server's closed
 * refusal text in `error` and no response fields.
 */
export type AppRestInvokeResult = {
  status?: number;
  content_type?: string;
  body_base64?: string;
  is_error: boolean;
  error?: string;
};

export type { AppInvokeRefusalKind };

const APP_INVOKE_REFUSAL_KINDS: readonly AppInvokeRefusalKind[] = [
  "app_not_found",
  "not_pinned",
  "consent_required",
  "unknown_tool",
];

/**
 * A typed refusal from `POST /apps/{id}/invoke`: the one invoke failure the
 * renderer branches on (`consent_required` re-opens the grant sheet) rather
 * than merely reporting.
 */
export class AppInvokeRefusalError extends Error {
  constructor(
    readonly kind: AppInvokeRefusalKind,
    message: string,
  ) {
    super(message);
    this.name = "AppInvokeRefusalError";
  }
}


/** Approval kinds a human may approve from the renderer. */
export function isApprovableKind(kind: RendererApprovalKind): boolean {
  return (
    kind === "search_may_share_query_and_excerpts" ||
    kind === "web_search_may_share_query" ||
    kind === "web_extract_may_fetch_url" ||
    kind === "exec_may_run_networked_command" ||
    kind === "external_mcp_may_call_server" ||
    kind === "workspace_may_modify_files"
  );
}

/** Approval kinds whose authority is stable enough to remember by tool name. */
export function isRememberableKind(kind: RendererApprovalKind): boolean {
  return isApprovableKind(kind) && kind !== "external_mcp_may_call_server";
}

/** A strict renderer-safe snapshot used to recover a parked approval. */
export type PendingToolApproval = {
  callId: string;
  turnId: string;
  action: RendererToolName;
  approval: RendererApprovalKind;
  class: "read_only" | "workspace" | "sensitive";
  /** What the parked call will do, when its tool projects a preview. */
  preview: ToolActionPreview | null;
  canApprove: boolean;
  canRemember: boolean;
  /** Complete standing-grant ladder the server will honor for this call. */
  grantRungs: ApprovalGrantRung[];
  /** Where the Auto-mode judge stands, or null when no judge was engaged. */
  autoJudgeStatus: "judging" | "approved" | "declined" | null;
};

export type FolderAccessHint = "documents" | "downloads";

/** A validated, pending request that the renderer may safely present. */
export type PendingFolderAccessRequest = {
  callId: string;
  turnId: string;
  reason: string;
  folderHint: FolderAccessHint | null;
  claimedByDesktop: boolean;
};

/** Renderer-safe replacement prompt. Canonical output, root, and path stay native. */
export type PendingOutputWritebackRequest = {
  callId: string;
  turnId: string;
  claimedByDesktop: boolean;
};

/**
 * One thing waiting on the reader, wherever it parked.
 *
 * Deliberately as opaque as the per-chat attention summary: enough to triage
 * and to navigate back, never the question, the plan, or the arguments. Those
 * are read from the conversation the item points at, by the card that owns
 * them — which is also the only place the item can be answered.
 */
export type InboxItem = {
  chatId: string;
  chatTitle: string | null;
  turnId: string;
  callId: string;
  kind: InboxItemKind;
  action: RendererToolName | null;
  requestedAt: string;
};

/** Opaque prompt state used to mark another chat as needing attention. */
export type PendingChatPrompt = {
  chatId: string;
  questionCallIds: string[];
  planCallIds: string[];
  folderAccessCallIds: string[];
  outputWritebackCallIds: string[];
};

export type UserQuestionOption = {
  id: string;
  label: string;
  description: string;
};

export type UserQuestion = {
  id: string;
  header: string;
  question: string;
  options: UserQuestionOption[];
  questionType: WireUserQuestionType;
  allowFreeForm: boolean;
};

/** Closed renderer projection of one durable question continuation. */
export type PendingUserQuestions = {
  callId: string;
  turnId: string;
  questions: UserQuestion[];
  askedAt: string;
};

export type UserQuestionAnswer = {
  questionId: string;
  selectedOptionIds: string[];
  customAnswer?: string;
};

/** Closed renderer projection of one durable plan continuation. */
export type PendingPlanApproval = {
  callId: string;
  turnId: string;
  title: string;
  plan: string;
  proposedAt: string;
};

export type PlanDecision =
  | { decision: "accept" }
  | { decision: "reject"; feedback?: string };

/**
 * The only consent prose a folder-access prompt will render.
 *
 * `parseFolderAccessRequest` rejects any request whose `reason` is not
 * byte-identical to this, so no server-authored text can reach a consent prompt.
 * The server holds the same literal; exported so a test can compare the two
 * across the language boundary rather than trusting they match.
 */
export const RENDERER_FOLDER_ACCESS_REASON =
  "The assistant needs read access to files outside the folders connected to this conversation.";

const WS_HANDSHAKE = "openwave-v1";
const WS_TOKEN_PREFIX = "openwave-token.";

/** How far a source download has got, when its length is known. */
export type FileDownloadProgress = {
  loaded: number;
  total: number;
  /** `loaded` over `total`, as 0–100. */
  percentage: number;
};

/**
 * The size below which a transfer is not worth reporting on.
 *
 * A bar that appears and vanishes is worse than no bar, and a source under this
 * arrives in a couple of chunks — most of them from a sidecar on this machine.
 * Well under the 16 MB a source may be, so the files big enough to wait on do
 * still report.
 */
const PROGRESS_MIN_BYTES = 2 * 1024 * 1024;

/** Progress updates are worth at most one re-render per frame budget. */
const PROGRESS_THROTTLE_MS = 100;

/**
 * Rate-limit progress callbacks, always letting the last one through.
 *
 * Without the trailing call the bar can stop short of the end: the final chunk
 * usually lands inside the throttle window of the one before it.
 */
function throttle(
  report: (progress: FileDownloadProgress) => void,
): (progress: FileDownloadProgress) => void {
  let last = 0;
  return (progress) => {
    const now = Date.now();
    if (progress.loaded >= progress.total || now - last >= PROGRESS_THROTTLE_MS) {
      last = now;
      report(progress);
    }
  };
}

/**
 * A rejected response, carrying the status so a caller can tell why.
 *
 * The status is what separates "this is gone" from "we could not reach the
 * server": a panel that cannot tell those apart has to guess, and guessing
 * wrong tells a reader their file was deleted when it was not.
 */
export class HttpError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
    this.name = "HttpError";
  }
}

/** The server's own message for a failed response, or its status text. */
async function throwIfNotOk(response: Response): Promise<void> {
  if (response.ok) return;
  let detail = response.statusText;
  try {
    const body = (await response.json()) as { message?: string };
    if (body.message) detail = body.message;
  } catch {
    /* ignore */
  }
  throw new HttpError(response.status, `${response.status}: ${detail}`);
}

export class ApiClient {
  constructor(
    readonly baseUrl: string,
    readonly token: string,
  ) {}

  private headers(json = false): HeadersInit {
    const headers: Record<string, string> = {
      Authorization: `Bearer ${this.token}`,
    };
    if (json) headers["Content-Type"] = "application/json";
    return headers;
  }

  private async json<T>(
    path: string,
    init?: RequestInit,
    expectedStatus?: number,
  ): Promise<T> {
    const response = await fetch(`${this.baseUrl}${path}`, init);
    await throwIfNotOk(response);
    if (expectedStatus !== undefined && response.status !== expectedStatus) {
      throw new Error(
        `unexpected response status: expected ${expectedStatus}, received ${response.status}`,
      );
    }
    if (response.status === 204) return undefined as T;
    const text = await response.text();
    if (!text) return undefined as T;
    return JSON.parse(text) as T;
  }

  listProviders(): Promise<{ providers: ProviderInfo[] }> {
    return this.json("/providers", { headers: this.headers() });
  }

  putProvider(
    kind: ProviderKind,
    body: {
      enabled?: boolean;
      base_url?: string | null;
      vertex_location?: string | null;
      credential?:
        | { type: "api_key"; key: string }
        | { type: "service_account"; json: string };
      models?: CustomModelConfig[];
    },
  ): Promise<ProviderInfo> {
    return this.json(`/providers/${kind}`, {
      method: "PUT",
      headers: this.headers(true),
      body: JSON.stringify(body),
    });
  }

  deleteCredential(kind: ProviderKind): Promise<void> {
    return this.json(`/providers/${kind}/credential`, {
      method: "DELETE",
      headers: this.headers(),
    });
  }

  getVoiceTranscription(): Promise<VoiceTranscriptionInfo> {
    return this.json("/voice-transcription", { headers: this.headers() });
  }

  putVoiceTranscription(
    model: VoiceTranscriptionModel,
    localModel?: string,
  ): Promise<VoiceTranscriptionInfo> {
    return this.json("/voice-transcription", {
      method: "PUT",
      headers: this.headers(true),
      body: JSON.stringify({ model, local_model: localModel ?? null }),
    });
  }

  installLocalVoice(model: string): Promise<LocalVoiceInfo> {
    return this.json("/voice-transcription/install", {
      method: "POST",
      headers: this.headers(true),
      body: JSON.stringify({ model }),
    });
  }

  async transcribeVoice(audio: Blob): Promise<string> {
    const response = await fetch(`${this.baseUrl}/voice-transcription`, {
      method: "POST",
      headers: {
        ...this.headers(),
        "Content-Type": audio.type || "audio/webm",
      },
      body: audio,
    });
    await throwIfNotOk(response);
    return ((await response.json()) as { text: string }).text;
  }

  /**
   * The selectable catalog, plus one row per model role: what the user pinned
   * it to, and what it resolves to right now — the only way a client can name
   * what "default" or "automatic" means for a role.
   */
  listModels(): Promise<ModelCatalog> {
    return this.json("/models", { headers: this.headers() });
  }

  /**
   * Pin a model role to one model, or pass `null` to return it to automatic
   * resolution against the role's ordered defaults.
   */
  putModelRole(
    role: ModelRole,
    selection: ModelSelectionKey | null,
  ): Promise<ModelRoleInfo> {
    return this.json(`/models/roles/${role}`, {
      method: "PUT",
      headers: this.headers(true),
      body: JSON.stringify({ selection }),
    });
  }

  getSettings(): Promise<RuntimeSettings> {
    return this.json("/settings", { headers: this.headers() });
  }

  /**
   * Update runtime settings. A field absent leaves it unchanged, `null` resets
   * it to the server default, and a value sets it (matching the double-option
   * body the server expects).
   */
  putSettings(body: {
    model?: ModelSelectionKey | null;
    max_active_background_agents?: number;
  }): Promise<RuntimeSettings> {
    return this.json("/settings", {
      method: "PUT",
      headers: this.headers(true),
      body: JSON.stringify(body),
    });
  }

  getWebSearchConfig(): Promise<WebSearchConfigInfo> {
    return this.json("/web-search", { headers: this.headers() });
  }

  putWebSearchConfig(body: {
    provider?: WebSearchProviderKind | null;
    timeout_ms?: number;
    // Explicit null clears the configured instance URL; omitting the field
    // leaves it as it is.
    searxng_base_url?: string | null;
  }): Promise<WebSearchConfigInfo> {
    return this.json("/web-search", {
      method: "PUT",
      headers: this.headers(true),
      body: JSON.stringify(body),
    });
  }

  listWebSearchCredentials(): Promise<{
    credentials: WebSearchCredentialReadiness[];
  }> {
    return this.json("/web-search/credentials", { headers: this.headers() });
  }

  putWebSearchCredential(
    provider: WebSearchProviderKind,
    apiKey: string,
  ): Promise<WebSearchCredentialReadiness> {
    return this.json(`/web-search/credentials/${provider}`, {
      method: "PUT",
      headers: this.headers(true),
      body: JSON.stringify({ api_key: apiKey }),
    });
  }

  deleteWebSearchCredential(
    provider: WebSearchProviderKind,
  ): Promise<WebSearchCredentialReadiness> {
    return this.json(`/web-search/credentials/${provider}`, {
      method: "DELETE",
      headers: this.headers(),
    });
  }

  getCodeExecutionConfig(): Promise<CodeExecutionConfigInfo> {
    return this.json("/code-execution", { headers: this.headers() });
  }

  putCodeExecutionConfig(body: {
    provider?: CodeExecutionProviderKind | null;
    timeout_ms?: number;
    egress?: EgressConfig;
  }): Promise<CodeExecutionConfigInfo> {
    return this.json("/code-execution", {
      method: "PUT",
      headers: this.headers(true),
      body: JSON.stringify(body),
    });
  }

  listCodeExecutionCredentials(): Promise<{
    credentials: CodeExecutionCredentialReadiness[];
  }> {
    return this.json("/code-execution/credentials", {
      headers: this.headers(),
    });
  }

  putCodeExecutionCredential(
    provider: CodeExecutionProviderKind,
    apiKey: string,
  ): Promise<CodeExecutionCredentialReadiness> {
    return this.json(`/code-execution/credentials/${provider}`, {
      method: "PUT",
      headers: this.headers(true),
      body: JSON.stringify({ api_key: apiKey }),
    });
  }

  deleteCodeExecutionCredential(
    provider: CodeExecutionProviderKind,
  ): Promise<CodeExecutionCredentialReadiness> {
    return this.json(`/code-execution/credentials/${provider}`, {
      method: "DELETE",
      headers: this.headers(),
    });
  }

  listConnectedApps(): Promise<ConnectedAppsInfo> {
    return this.json("/connected-apps", { headers: this.headers() });
  }

  putRestConnectedApp(
    id: string,
    body: {
      name: string;
      base_url: string;
      /** The raw JSON OpenAPI document, when supplied inline; the server
       * ingests it once here. Exactly one of this and
       * `openapi_document_url` must be present. */
      openapi_document?: string;
      /** URL the server fetches the document from at save time. Requires
       * `document_sha256`. */
      openapi_document_url?: string;
      /** The preview's document hash pin; the save refuses (409) if the
       * document no longer matches it. */
      document_sha256?: string;
      /** When present, only these operationIds are ingested; the rest of
       * the document is not judged. */
      operation_ids?: string[];
      credential: RestCredentialUpdate;
    },
  ): Promise<ConnectedAppsInfo> {
    return this.json(`/connected-apps/rest/${encodeURIComponent(id)}`, {
      method: "PUT",
      headers: this.headers(true),
      body: JSON.stringify(body),
    });
  }

  /** List what an OpenAPI document declares, for the operation picker. */
  previewRestSpec(
    source: { url: string } | { document: string },
  ): Promise<SpecPreviewInfo> {
    return this.json("/connected-apps/rest/spec-preview", {
      method: "POST",
      headers: this.headers(true),
      body: JSON.stringify({ source }),
    });
  }

  deleteRestConnectedApp(id: string): Promise<void> {
    return this.json(`/connected-apps/rest/${encodeURIComponent(id)}`, {
      method: "DELETE",
      headers: this.headers(),
    });
  }

  listMcpServers(): Promise<McpServersInfo> {
    return this.json("/mcp/servers", { headers: this.headers() });
  }

  putMcpServers(servers: McpServerDefinition[]): Promise<McpServersInfo> {
    return this.json("/mcp/servers", {
      method: "PUT",
      headers: this.headers(true),
      body: JSON.stringify({ servers }),
    });
  }

  reconnectMcpServer(name: string): Promise<McpServersInfo> {
    return this.json(`/mcp/servers/${encodeURIComponent(name)}/reconnect`, {
      method: "POST",
      headers: this.headers(),
    });
  }

  /** Trade the bearer for a single-use iframe address for one view. */
  createMcpViewFrame(server: string, uri: string): Promise<McpViewSession> {
    return this.json(`/mcp/servers/${encodeURIComponent(server)}/view-session`, {
      method: "POST",
      headers: this.headers(true),
      body: JSON.stringify({ uri }),
    });
  }

  getPolicy(): Promise<ManagedPolicy> {
    return this.json("/policy", { headers: this.headers() });
  }

  getGatewayStatus(): Promise<GatewayStatus> {
    return this.json("/gateway/status", { headers: this.headers() });
  }

  gatewaySignIn(): Promise<{ authorization_url: string }> {
    return this.json("/gateway/sign-in", {
      method: "POST",
      headers: this.headers(),
    });
  }

  gatewaySignOut(): Promise<GatewayStatus> {
    return this.json("/gateway/sign-out", {
      method: "POST",
      headers: this.headers(),
    });
  }

  /** Decline the pending deep-link pairing; returns the policy to render. */
  dismissGatewayPairing(): Promise<ManagedPolicy> {
    return this.json("/gateway/pairing/dismiss", {
      method: "POST",
      headers: this.headers(),
    });
  }

  getGatewayApps(): Promise<GatewayApps> {
    return this.json("/gateway/apps", { headers: this.headers() });
  }

  syncGatewayModels(): Promise<GatewayStatus> {
    return this.json("/gateway/models/sync", {
      method: "POST",
      headers: this.headers(),
    });
  }

  getMcpAppPayload(chatId: string, callId: string): Promise<McpAppPayload> {
    return this.json(
      `/chats/${encodeURIComponent(chatId)}/calls/${encodeURIComponent(callId)}/mcp-app-payload`,
      { headers: this.headers() },
    );
  }

  listApps(): Promise<AppLibrary> {
    return this.json("/apps", { headers: this.headers() });
  }

  getApp(appId: string): Promise<AppDetail> {
    return this.json(`/apps/${encodeURIComponent(appId)}`, {
      headers: this.headers(),
    });
  }

  deleteApp(appId: string): Promise<void> {
    return this.json(`/apps/${encodeURIComponent(appId)}`, {
      method: "DELETE",
      headers: this.headers(),
    });
  }

  getAppGrant(appId: string): Promise<AppGrantState> {
    return this.json(`/apps/${encodeURIComponent(appId)}/grant`, {
      headers: this.headers(),
    });
  }

  /**
   * Record consent. Deliberately body-less: consent is only ever "yes to what
   * the server shows right now" — the server recomputes the grant from the
   * current manifest and definitions, so a stale sheet can never widen it.
   */
  consentAppGrant(appId: string): Promise<AppGrantState> {
    return this.json(`/apps/${encodeURIComponent(appId)}/grant`, {
      method: "POST",
      headers: this.headers(),
    });
  }

  revokeAppGrant(appId: string): Promise<void> {
    return this.json(`/apps/${encodeURIComponent(appId)}/grant`, {
      method: "DELETE",
      headers: this.headers(),
    });
  }

  /** Trade the bearer for a single-use iframe address for one app revision. */
  createAppViewFrame(appId: string): Promise<AppViewSession> {
    return this.json(`/apps/${encodeURIComponent(appId)}/view-session`, {
      method: "POST",
      headers: this.headers(true),
      body: JSON.stringify({}),
    });
  }

  /**
   * Execute one of an app's pinned tools outside any turn. `args` and the
   * result are opaque passthrough between the sandboxed frame and the server;
   * a typed refusal surfaces as {@link AppInvokeRefusalError} so the caller
   * can branch on `consent_required` without string-matching prose.
   */
  async invokeApp(
    appId: string,
    tool: string,
    args: unknown,
  ): Promise<AppInvokeResult> {
    return (await this.postAppInvoke(appId, {
      tool,
      arguments: args,
    })) as AppInvokeResult;
  }

  /**
   * Execute one of an app's pinned REST operations outside any turn — the
   * `operation_id` sibling of {@link invokeApp}, with the same opaque
   * passthrough and refusal contract. The response body crosses base64-
   * encoded in `body_base64` (see {@link AppRestInvokeResult}).
   */
  async invokeAppOperation(
    appId: string,
    operationId: string,
    parameters?: unknown,
    body?: unknown,
  ): Promise<AppRestInvokeResult> {
    const request: Record<string, unknown> = { operation_id: operationId };
    if (parameters !== undefined) request.parameters = parameters;
    if (body !== undefined) request.body = body;
    return (await this.postAppInvoke(appId, request)) as AppRestInvokeResult;
  }

  /** The shared invoke POST: one route, typed refusals surfaced as errors. */
  private async postAppInvoke(appId: string, request: unknown): Promise<unknown> {
    const response = await fetch(
      `${this.baseUrl}/apps/${encodeURIComponent(appId)}/invoke`,
      {
        method: "POST",
        headers: this.headers(true),
        body: JSON.stringify(request),
      },
    );
    if (!response.ok) {
      let refusal: unknown;
      try {
        refusal = await response.clone().json();
      } catch {
        /* not a typed refusal; fall through to the generic error */
      }
      if (
        typeof refusal === "object" &&
        refusal !== null &&
        "kind" in refusal &&
        "message" in refusal &&
        APP_INVOKE_REFUSAL_KINDS.includes(
          (refusal as { kind: AppInvokeRefusalKind }).kind,
        )
      ) {
        const typed = refusal as { kind: AppInvokeRefusalKind; message: string };
        throw new AppInvokeRefusalError(typed.kind, String(typed.message));
      }
      await throwIfNotOk(response);
    }
    return await response.json();
  }

  createProject(title: string): Promise<Project> {
    return this.json("/projects", {
      method: "POST",
      headers: this.headers(true),
      body: JSON.stringify({ title }),
    });
  }

  listProjects(): Promise<Project[]> {
    return this.json("/projects", { headers: this.headers() });
  }

  patchProjectTitle(projectId: string, title: string | null): Promise<Project> {
    return this.json(`/projects/${encodeURIComponent(projectId)}`, {
      method: "PATCH",
      headers: this.headers(true),
      body: JSON.stringify({ title }),
    });
  }

  deleteProject(projectId: string): Promise<void> {
    return this.json(`/projects/${encodeURIComponent(projectId)}`, {
      method: "DELETE",
      headers: this.headers(),
    });
  }

  /**
   * Create a chat, optionally already set up the way it will run.
   *
   * The turn inputs are sent at creation rather than PATCHed afterwards: a
   * correcting PATCH races the first turn, which reads the chat as it was
   * created.
   */
  createChat(
    model?: ModelSelectionKey,
    projectId?: string | null,
    settings?: {
      reasoningEffort?: ReasoningEffort | null;
      permissionMode?: PermissionMode | null;
      networkPolicy?: NetworkPolicy;
    },
  ): Promise<Chat> {
    return this.json("/chats", {
      method: "POST",
      headers: this.headers(true),
      body: JSON.stringify({
        model: model || undefined,
        project_id: projectId || undefined,
        reasoning_effort: settings?.reasoningEffort ?? undefined,
        permission_mode: settings?.permissionMode ?? undefined,
        network_policy: settings?.networkPolicy,
      }),
    });
  }

  listChats(): Promise<Chat[]> {
    return this.json("/chats", { headers: this.headers() });
  }

  getChat(chatId: string): Promise<Chat> {
    return this.json(`/chats/${chatId}`, { headers: this.headers() });
  }

  /**
   * Everything parked on this reader, across their conversations.
   *
   * One server-side read rather than a loop over chats: the shell needs the
   * whole set to badge the inbox and mark the rail, and asking each chat in
   * turn would make that cost grow with the profile.
   */
  async listInbox(): Promise<InboxItem[]> {
    const body = await this.json<unknown>("/inbox", { headers: this.headers() });
    if (!Array.isArray(body)) {
      throw new Error("inbox response is not an array");
    }
    const items: InboxItem[] = [];
    const seen = new Set<string>();
    for (const value of body) {
      const item = parseInboxItem(value);
      if (!item || seen.has(item.callId)) {
        throw new Error("inbox response contains invalid data");
      }
      seen.add(item.callId);
      items.push(item);
    }
    return items;
  }

  async listPendingChatPrompts(): Promise<PendingChatPrompt[]> {
    const body = await this.json<unknown>("/chats/pending-prompts", {
      headers: this.headers(),
    });
    if (!Array.isArray(body)) {
      throw new Error("pending chat prompt response is not an array");
    }
    const prompts = new Map<string, PendingChatPrompt>();
    for (const item of body) {
      const prompt = parsePendingChatPrompt(item);
      if (!prompt || prompts.has(prompt.chatId)) {
        throw new Error("pending chat prompt response contains invalid data");
      }
      prompts.set(prompt.chatId, prompt);
    }
    return [...prompts.values()];
  }

  deleteChat(chatId: string): Promise<void> {
    return this.json(`/chats/${chatId}`, {
      method: "DELETE",
      headers: this.headers(),
    });
  }

  listChatMessages(chatId: string): Promise<ChatTranscript> {
    return this.json(`/chats/${chatId}/messages`, {
      headers: this.headers(),
    });
  }

  undoTurnFileChanges(
    chatId: string,
    turnId: string,
  ): Promise<{ chat_id: string; turn_id: string; files: ExecFileUndoOutcome[] }> {
    return this.json(
      `/chats/${encodeURIComponent(chatId)}/turns/${encodeURIComponent(turnId)}/file-changes/undo`,
      { method: "POST", headers: this.headers() },
    );
  }

  undoFileChange(
    chatId: string,
    turnId: string,
    snapshotId: string,
  ): Promise<ExecFileUndoOutcome> {
    return this.json(
      `/chats/${encodeURIComponent(chatId)}/turns/${encodeURIComponent(turnId)}/file-changes/${encodeURIComponent(snapshotId)}/undo`,
      { method: "POST", headers: this.headers() },
    );
  }

  getFileChangePreview(
    chatId: string,
    turnId: string,
    snapshotId: string,
    revision: "before" | "after",
    signal?: AbortSignal,
  ): Promise<Blob> {
    return this.blob(
      `/chats/${encodeURIComponent(chatId)}/turns/${encodeURIComponent(turnId)}/file-changes/${encodeURIComponent(snapshotId)}/preview/${revision}`,
      signal,
    );
  }

  private async blob(path: string, signal?: AbortSignal): Promise<Blob> {
    const response = await fetch(`${this.baseUrl}${path}`, {
      headers: this.headers(),
      signal,
    });
    await throwIfNotOk(response);
    return response.blob();
  }

  /**
   * Bytes read as they arrive, reporting how much has landed.
   *
   * Reading the body as a stream rather than awaiting it whole is the only way
   * to say anything about a transfer while it is still running. It costs an
   * extra copy — the chunks are joined once at the end — which is why the
   * callers that have nothing to report progress to still use {@link blob}.
   *
   * `onProgress` is only ever called when the response declares its length:
   * without a total there is no fraction to report, and a byte count climbing
   * toward an unknown end is not worth a progress bar.
   */
  private async streamBytes(
    path: string,
    signal?: AbortSignal,
    onProgress?: (progress: FileDownloadProgress) => void,
  ): Promise<{ bytes: Uint8Array; contentType: string | null }> {
    const response = await fetch(`${this.baseUrl}${path}`, {
      headers: this.headers(),
      signal,
    });
    await throwIfNotOk(response);

    const contentType = response.headers.get("Content-Type");
    const declared = Number(response.headers.get("Content-Length"));
    const total = Number.isSafeInteger(declared) && declared > 0 ? declared : 0;

    // No reader to stream from (an old runtime, or a mocked response in a
    // test): take the whole body and skip straight to the finished state.
    if (!response.body) {
      const bytes = new Uint8Array(await response.arrayBuffer());
      return { bytes, contentType };
    }

    const report =
      onProgress && total > PROGRESS_MIN_BYTES ? throttle(onProgress) : null;
    const reader = response.body.getReader();
    const chunks: Uint8Array[] = [];
    let loaded = 0;

    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      chunks.push(value);
      loaded += value.length;
      report?.({ loaded, total, percentage: (loaded / total) * 100 });
    }

    const bytes = new Uint8Array(loaded);
    let offset = 0;
    for (const chunk of chunks) {
      bytes.set(chunk, offset);
      offset += chunk.length;
    }
    return { bytes, contentType };
  }

  getChatImageAttachment(
    chatId: string,
    attachmentId: string,
    signal?: AbortSignal,
  ): Promise<Blob> {
    return this.blob(
      `/chats/${encodeURIComponent(chatId)}/attachments/images/${encodeURIComponent(attachmentId)}`,
      signal,
    );
  }

  /** One source's extracted text and catalog metadata. */
  getChatDocument(chatId: string, documentId: string): Promise<DocumentDetail> {
    return this.json(
      `/chats/${encodeURIComponent(chatId)}/documents/${encodeURIComponent(documentId)}`,
      { headers: this.headers() },
    );
  }

  /**
   * The original bytes of one source, exactly as they were imported.
   *
   * Bytes are addressed by document id inside its conversation and never by a
   * host path, so a viewer can show the file the reader gave us without the
   * renderer learning where on disk it came from. The conversation is part of
   * the address rather than decoration: the server serves a document's bytes
   * only under the chat that owns it.
   *
   * Returned as bytes rather than a URL because the renderer authenticates with
   * a bearer header the webview cannot attach to an `<embed>` or `<img>` source,
   * and because pdf.js and the workbook parsers want a buffer anyway. The
   * stored media type comes back alongside them because it is what the text
   * viewers dispatch on, and it would otherwise be lost when the streamed
   * chunks are reassembled.
   */
  getChatDocumentFile(
    chatId: string,
    documentId: string,
    signal?: AbortSignal,
    onProgress?: (progress: FileDownloadProgress) => void,
  ): Promise<{ bytes: Uint8Array; contentType: string | null }> {
    return this.streamBytes(
      `/chats/${encodeURIComponent(chatId)}/documents/${encodeURIComponent(documentId)}/file-content`,
      signal,
      onProgress,
    );
  }

  patchChatTitle(chatId: string, title: string | null): Promise<Chat> {
    return this.json(`/chats/${chatId}`, {
      method: "PATCH",
      headers: this.headers(true),
      body: JSON.stringify({ title }),
    });
  }

  patchChatModel(
    chatId: string,
    model: ModelSelectionKey | null,
  ): Promise<Chat> {
    return this.json(`/chats/${chatId}`, {
      method: "PATCH",
      headers: this.headers(true),
      body: JSON.stringify({ model }),
    });
  }

  patchChatReasoningEffort(
    chatId: string,
    reasoningEffort: ReasoningEffort | null,
  ): Promise<Chat> {
    return this.json(`/chats/${chatId}`, {
      method: "PATCH",
      headers: this.headers(true),
      body: JSON.stringify({ reasoning_effort: reasoningEffort }),
    });
  }

  patchChatPermissionMode(
    chatId: string,
    permissionMode: PermissionMode | null,
  ): Promise<Chat> {
    return this.json(`/chats/${chatId}`, {
      method: "PATCH",
      headers: this.headers(true),
      body: JSON.stringify({ permission_mode: permissionMode }),
    });
  }

  patchChatNetworkPolicy(
    chatId: string,
    networkPolicy: NetworkPolicy,
  ): Promise<Chat> {
    return this.json(`/chats/${chatId}`, {
      method: "PATCH",
      headers: this.headers(true),
      body: JSON.stringify({ network_policy: networkPolicy }),
    });
  }

  listAgentRuns(chatId: string): Promise<AgentRun[]> {
    return this.json(`/chats/${chatId}/agent-runs`, {
      headers: this.headers(),
    });
  }

  /**
   * The ordered, renderer-safe activity history for one background run.
   *
   * Malformed or unknown entries are dropped rather than trusted, keeping the
   * closed vocabulary the server promises. A wrong-chat, foreground, or missing
   * run answers `404`, which surfaces as a thrown error.
   */
  async listAgentRunActivity(
    chatId: string,
    runId: string,
  ): Promise<AgentActivityHistoryEntry[]> {
    const body = await this.json<unknown>(
      `/chats/${encodeURIComponent(chatId)}/agent-runs/${encodeURIComponent(runId)}/activity`,
      { headers: this.headers() },
    );
    return parseAgentActivityHistory(body);
  }

  async cancelAgentRun(
    chatId: string,
    runId: string,
  ): Promise<SandboxAgentCancellation> {
    const body = await this.json<unknown>(
      `/chats/${encodeURIComponent(chatId)}/agent-runs/${encodeURIComponent(runId)}/cancel`,
      {
        method: "POST",
        headers: this.headers(),
      },
      202,
    );
    const cancellation = parseSandboxAgentCancellation(body);
    if (!cancellation || cancellation.id !== runId) {
      throw new Error("sandbox cancellation response is invalid");
    }
    return cancellation;
  }

  /**
   * `attachments` names images already published for this chat, in the order
   * they should be shown to the model. Only identity crosses: the server
   * re-derives every attachment's format and dimensions from the stored bytes.
   */
  postMessage(
    chatId: string,
    turnId: string,
    content: string,
    attachments: readonly string[] = [],
    fileAttachments: readonly string[] = [],
  ): Promise<void> {
    return this.json(`/chats/${chatId}/messages`, {
      method: "POST",
      headers: this.headers(true),
      body: JSON.stringify({
        turn_id: turnId,
        content,
        attachments,
        file_attachments: fileAttachments,
      }),
    });
  }

  steer(
    chatId: string,
    turnId: string,
    steerId: string,
    content: string,
    interrupt = false,
  ): Promise<void> {
    return this.json(`/chats/${chatId}/steer`, {
      method: "POST",
      headers: this.headers(true),
      body: JSON.stringify({ steer_id: steerId, turn_id: turnId, content, interrupt }),
    });
  }

  cancel(chatId: string, turnId: string): Promise<void> {
    return this.json(`/chats/${chatId}/cancel`, {
      method: "POST",
      headers: this.headers(true),
      body: JSON.stringify({ turn_id: turnId }),
    });
  }

  decideApproval(
    chatId: string,
    callId: string,
    decision: "approve" | "reject",
    grant: ApprovalGrantRung | null = null,
  ): Promise<void> {
    return this.json(`/chats/${chatId}/approvals/${callId}`, {
      method: "POST",
      headers: this.headers(true),
      body: JSON.stringify({ decision, grant }),
    });
  }

  async listPendingApprovals(chatId: string): Promise<PendingToolApproval[]> {
    const body = await this.json<unknown>(`/chats/${chatId}/approvals`, {
      headers: this.headers(),
    });
    if (!Array.isArray(body)) {
      throw new Error("pending approval response is not an array");
    }

    const approvals = new Map<string, PendingToolApproval>();
    let turnId: string | null = null;
    for (const item of body) {
      const approval = parsePendingToolApproval(item);
      if (!approval) {
        throw new Error("pending approval response contains an invalid item");
      }
      if (approvals.has(approval.callId)) {
        throw new Error("pending approval response contains a duplicate call");
      }
      if (turnId !== null && turnId !== approval.turnId) {
        throw new Error("pending approval response spans multiple turns");
      }
      turnId = approval.turnId;
      approvals.set(approval.callId, approval);
    }
    return [...approvals.values()];
  }

  /** Every standing "don't ask again", newest first, across all chats. */
  listStandingGrants(): Promise<StandingGrantSnapshot[]> {
    return this.json(`/grants`, { headers: this.headers() });
  }

  /**
   * The server's rows of the unified consent read model: every standing tool
   * grant as one consent statement. The capability half comes from the host
   * broker over the Tauri boundary and joins these rows renderer-side.
   */
  listConsentStatements(): Promise<ConsentStatementSnapshot[]> {
    return this.json(`/consent/statements`, { headers: this.headers() });
  }

  /** Withdraw a standing grant; later matching calls ask again. */
  revokeStandingGrant(sourceCallId: string): Promise<void> {
    return this.json(`/grants/${sourceCallId}`, {
      method: "DELETE",
      headers: this.headers(),
    });
  }

  async listPendingFolderAccessRequests(
    chatId: string,
  ): Promise<PendingFolderAccessRequest[]> {
    const body = await this.json<unknown>(
      `/chats/${chatId}/client-executions/pending`,
      { headers: this.headers() },
    );
    if (!Array.isArray(body)) return [];

    const requests = new Map<string, PendingFolderAccessRequest>();
    for (const item of body) {
      const request = parseFolderAccessRequest(item);
      if (request && !requests.has(request.callId)) {
        requests.set(request.callId, request);
      }
    }
    return [...requests.values()];
  }

  async listPendingOutputWritebackRequests(
    chatId: string,
  ): Promise<PendingOutputWritebackRequest[]> {
    const body = await this.json<unknown>(
      `/chats/${chatId}/output-writebacks/pending`,
      { headers: this.headers() },
    );
    if (!Array.isArray(body)) {
      throw new Error("pending output write-back response is not an array");
    }

    const requests = new Map<string, PendingOutputWritebackRequest>();
    for (const item of body) {
      const request = parseOutputWritebackRequest(item);
      if (!request || requests.has(request.callId)) {
        throw new Error("pending output write-back response contains invalid data");
      }
      requests.set(request.callId, request);
    }
    return [...requests.values()];
  }

  async listPendingUserQuestions(
    chatId: string,
  ): Promise<PendingUserQuestions[]> {
    const body = await this.json<unknown>(`/chats/${chatId}/questions/pending`, {
      headers: this.headers(),
    });
    if (!Array.isArray(body)) {
      throw new Error("pending question response is not an array");
    }
    const requests = new Map<string, PendingUserQuestions>();
    for (const item of body) {
      const request = parsePendingUserQuestions(item);
      if (!request || requests.has(request.callId)) {
        throw new Error("pending question response contains invalid data");
      }
      requests.set(request.callId, request);
    }
    return [...requests.values()];
  }

  async answerUserQuestions(
    chatId: string,
    callId: string,
    answers: UserQuestionAnswer[],
    additionalUserContext?: string,
  ): Promise<void> {
    await this.json(`/chats/${chatId}/questions/${callId}/answer`, {
      method: "POST",
      headers: this.headers(true),
      body: JSON.stringify({
        answers: answers.map((answer) => ({
          question_id: answer.questionId,
          selected_option_ids: answer.selectedOptionIds,
          ...(answer.customAnswer === undefined
            ? {}
            : { custom_answer: answer.customAnswer }),
        })),
        ...(additionalUserContext === undefined
          ? {}
          : { additional_user_context: additionalUserContext }),
      }),
    });
  }

  async listPendingPlanApprovals(
    chatId: string,
  ): Promise<PendingPlanApproval[]> {
    const body = await this.json<unknown>(`/chats/${chatId}/plans/pending`, {
      headers: this.headers(),
    });
    if (!Array.isArray(body)) {
      throw new Error("pending plan response is not an array");
    }
    const requests = new Map<string, PendingPlanApproval>();
    for (const item of body) {
      const request = parsePendingPlanApproval(item);
      if (!request || requests.has(request.callId)) {
        throw new Error("pending plan response contains invalid data");
      }
      requests.set(request.callId, request);
    }
    return [...requests.values()];
  }

  async decidePlan(
    chatId: string,
    callId: string,
    decision: PlanDecision,
  ): Promise<void> {
    await this.json(`/chats/${chatId}/plans/${callId}/decision`, {
      method: "POST",
      headers: this.headers(true),
      body: JSON.stringify(
        decision.decision === "reject" && decision.feedback !== undefined
          ? { decision: "reject", feedback: decision.feedback }
          : { decision: decision.decision },
      ),
    });
  }

  /** Open the chat event stream; auth via Sec-WebSocket-Protocol. */
  openEvents(chatId: string, after: number, onFrame: (frame: ChatFrame) => void): WebSocket {
    const url = `${this.baseUrl.replace(/^http/, "ws")}/chats/${chatId}/events?after=${after}`;
    const protocols = [WS_HANDSHAKE, `${WS_TOKEN_PREFIX}${this.token}`];
    const socket = new WebSocket(url, protocols);
    socket.onmessage = (msg) => {
      try {
        onFrame(JSON.parse(String(msg.data)) as ChatFrame);
      } catch (err) {
        console.error("bad event frame", err);
      }
    };
    return socket;
  }
}

export function parseFolderAccessRequest(
  value: unknown,
): PendingFolderAccessRequest | null {
  if (!isRecord(value)) return null;
  if (
    !onlyKeys<WirePendingFolderAccessRequest>(value, [
      "call_id",
      "turn_id",
      "reason",
      "folder_hint",
      "claimed",
    ]) ||
    typeof value.call_id !== "string" ||
    value.call_id.length === 0 ||
    typeof value.turn_id !== "string" ||
    value.turn_id.length === 0 ||
    typeof value.reason !== "string" ||
    value.reason !== RENDERER_FOLDER_ACCESS_REASON ||
    typeof value.claimed !== "boolean"
  ) {
    return null;
  }
  const folderHint = value.folder_hint;
  if (
    folderHint !== null &&
    folderHint !== "documents" &&
    folderHint !== "downloads"
  ) {
    return null;
  }

  return {
    callId: value.call_id,
    turnId: value.turn_id,
    reason: value.reason,
    folderHint,
    claimedByDesktop: value.claimed,
  };
}

export function parseOutputWritebackRequest(
  value: unknown,
): PendingOutputWritebackRequest | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WirePendingOutputWritebackRequest>(value, [
      "call_id",
      "turn_id",
      "claimed",
    ]) ||
    !nonEmptyBounded(value.call_id, 128) ||
    !nonEmptyBounded(value.turn_id, 128) ||
    typeof value.claimed !== "boolean"
  ) {
    return null;
  }
  return {
    callId: value.call_id,
    turnId: value.turn_id,
    claimedByDesktop: value.claimed,
  };
}

/**
 * Validate the intentionally small parked-chat summary before it reaches
 * shared shell state. Details belong to the selected chat's recovery route,
 * never to the list indicator.
 */
const INBOX_ITEM_KINDS = new Set<InboxItemKind>([
  "tool_approval",
  "question",
  "plan_review",
  "folder_access",
  "output_writeback",
]);

export function parseInboxItem(value: unknown): InboxItem | null {
  if (
    !isRecord(value) ||
    !onlyKeys<{
      chat_id: string;
      chat_title?: string;
      turn_id: string;
      call_id: string;
      kind: InboxItemKind;
      action?: RendererToolName;
      requested_at: string;
    }>(value, [
      "chat_id",
      "chat_title",
      "turn_id",
      "call_id",
      "kind",
      "action",
      "requested_at",
    ]) ||
    !nonEmptyBounded(value.chat_id, 128) ||
    !nonEmptyBounded(value.turn_id, 128) ||
    !nonEmptyBounded(value.call_id, 128) ||
    !nonEmptyBounded(value.requested_at, 64) ||
    typeof value.kind !== "string" ||
    !INBOX_ITEM_KINDS.has(value.kind as InboxItemKind)
  ) {
    return null;
  }
  // Both are optional on the wire, and neither may arrive as anything but its
  // own declared shape — an untitled chat omits the key rather than sending an
  // empty title, and only the closed tool vocabulary may name an action.
  if (
    value.chat_title !== undefined &&
    !nonEmptyBounded(value.chat_title, 256)
  ) {
    return null;
  }
  if (
    value.action !== undefined &&
    !RENDERER_TOOL_NAMES.includes(value.action as RendererToolName)
  ) {
    return null;
  }
  return {
    chatId: value.chat_id,
    chatTitle: (value.chat_title as string | undefined) ?? null,
    turnId: value.turn_id,
    callId: value.call_id,
    kind: value.kind as InboxItemKind,
    action: (value.action as RendererToolName | undefined) ?? null,
    requestedAt: value.requested_at,
  };
}

export function parsePendingChatPrompt(value: unknown): PendingChatPrompt | null {
  if (
    !isRecord(value) ||
    !onlyKeys<{
      chat_id: string;
      question_call_ids: string[];
      plan_call_ids: string[];
      folder_access_call_ids: string[];
      output_writeback_call_ids: string[];
    }>(value, [
      "chat_id",
      "question_call_ids",
      "plan_call_ids",
      "folder_access_call_ids",
      "output_writeback_call_ids",
    ]) ||
    !nonEmptyBounded(value.chat_id, 128)
  ) {
    return null;
  }
  const questionCallIds = parseOpaqueCallIds(value.question_call_ids);
  const planCallIds = parseOpaqueCallIds(value.plan_call_ids);
  const folderAccessCallIds = parseOpaqueCallIds(value.folder_access_call_ids);
  const outputWritebackCallIds = parseOpaqueCallIds(
    value.output_writeback_call_ids,
  );
  if (
    !questionCallIds ||
    !planCallIds ||
    !folderAccessCallIds ||
    !outputWritebackCallIds
  ) {
    return null;
  }
  const total =
    questionCallIds.length +
    planCallIds.length +
    folderAccessCallIds.length +
    outputWritebackCallIds.length;
  if (
    total === 0 ||
    new Set([
      ...questionCallIds,
      ...planCallIds,
      ...folderAccessCallIds,
      ...outputWritebackCallIds,
    ]).size !== total
  ) {
    return null;
  }
  return {
    chatId: value.chat_id,
    questionCallIds,
    planCallIds,
    folderAccessCallIds,
    outputWritebackCallIds,
  };
}

function parseOpaqueCallIds(value: unknown): string[] | null {
  if (!Array.isArray(value) || value.length > 64) return null;
  const callIds = new Set<string>();
  for (const callId of value) {
    if (!nonEmptyBounded(callId, 128) || callIds.has(callId)) return null;
    callIds.add(callId);
  }
  return [...callIds];
}

export function parsePendingPlanApproval(
  value: unknown,
): PendingPlanApproval | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WirePendingPlanApproval>(value, [
      "call_id",
      "turn_id",
      "title",
      "plan",
      "proposed_at",
    ]) ||
    !nonEmptyBounded(value.call_id, 128) ||
    !nonEmptyBounded(value.turn_id, 128) ||
    !nonEmptyBounded(value.title, 120) ||
    typeof value.plan !== "string" ||
    !value.plan.trim() ||
    Array.from(value.plan).length > 40_000 ||
    !nonEmptyBounded(value.proposed_at, 64)
  ) {
    return null;
  }
  return {
    callId: value.call_id,
    turnId: value.turn_id,
    title: value.title,
    plan: value.plan,
    proposedAt: value.proposed_at,
  };
}

export function parsePendingUserQuestions(
  value: unknown,
): PendingUserQuestions | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WirePendingUserQuestions>(value, [
      "call_id",
      "turn_id",
      "questions",
      "asked_at",
    ]) ||
    !nonEmptyBounded(value.call_id, 128) ||
    !nonEmptyBounded(value.turn_id, 128) ||
    typeof value.asked_at !== "string" ||
    value.asked_at.length > 64 ||
    !Array.isArray(value.questions) ||
    value.questions.length < 1 ||
    value.questions.length > 3
  ) {
    return null;
  }
  const questions: UserQuestion[] = [];
  const questionIds = new Set<string>();
  for (const item of value.questions) {
    if (
      !isRecord(item) ||
      !onlyKeys<WireUserQuestion>(item, [
        "id",
        "header",
        "question",
        "options",
        "question_type",
        "allow_free_form",
      ]) ||
      !nonEmptyBounded(item.id, 64) ||
      questionIds.has(item.id) ||
      !nonEmptyBounded(item.header, 32) ||
      !nonEmptyBounded(item.question, 500) ||
      !Array.isArray(item.options) ||
      item.options.length > 5 ||
      (item.question_type !== "single_select" &&
        item.question_type !== "multi_select") ||
      typeof item.allow_free_form !== "boolean" ||
      (item.options.length === 0 && !item.allow_free_form)
    ) {
      return null;
    }
    questionIds.add(item.id);
    const options: UserQuestionOption[] = [];
    const optionIds = new Set<string>();
    for (const option of item.options) {
      if (
        !isRecord(option) ||
        !onlyKeys<WireUserQuestionOption>(option, ["id", "label", "description"]) ||
        !nonEmptyBounded(option.id, 64) ||
        optionIds.has(option.id) ||
        !nonEmptyBounded(option.label, 80) ||
        !nonEmptyBounded(option.description, 240)
      ) {
        return null;
      }
      optionIds.add(option.id);
      options.push({
        id: option.id,
        label: option.label,
        description: option.description,
      });
    }
    questions.push({
      id: item.id,
      header: item.header,
      question: item.question,
      options,
      questionType: item.question_type,
      allowFreeForm: item.allow_free_form,
    });
  }
  return {
    callId: value.call_id,
    turnId: value.turn_id,
    questions,
    askedAt: value.asked_at,
  };
}

/**
 * Whether `value` carries no key outside `allowed`.
 *
 * Generic over the wire type so the allowlist has to be spelled with that type's
 * own keys: a field renamed in Rust drops out of `keyof` and the call below
 * fails to compile. Without that, a rename left the allowlist naming the old key
 * and rejecting the new one, so the validator would reject every payload and the
 * surface would simply stop appearing — with nothing failing.
 */
function onlyKeys<Wire>(
  value: Record<string, unknown>,
  allowed: readonly (keyof Wire & string)[],
): boolean {
  const set = new Set<string>(allowed);
  return Object.keys(value).every((key) => set.has(key));
}

function nonEmptyBounded(value: unknown, maxChars: number): value is string {
  return (
    typeof value === "string" &&
    value.trim().length > 0 &&
    Array.from(value).length <= maxChars &&
    !Array.from(value).some((character) => {
      const code = character.codePointAt(0) ?? 0;
      return code < 32 || (code >= 127 && code <= 159);
    })
  );
}

const AGENT_ACTIVITY_KINDS = new Set<AgentActivityKind>([
  "web_search",
  "read_delegated_file",
  "list_connected_folders",
  "list_folder",
  "read_connected_file",
  "import_connected_file",
]);

const AGENT_ACTIVITY_OUTCOMES = new Set<AgentActivityOutcome>([
  "waiting",
  "running",
  "completed",
  "failed",
  "cancelled",
]);

/**
 * Keep only well-formed history entries in their server order. An entry whose
 * kind or outcome falls outside the closed vocabulary, or whose timestamp is
 * missing, is dropped rather than rendered — the same defensive discipline the
 * transcript applies to every model-influenced projection.
 */
export function parseAgentActivityHistory(
  value: unknown,
): AgentActivityHistoryEntry[] {
  if (!Array.isArray(value)) return [];
  return value.flatMap((entry) => {
    if (
      !isRecord(entry) ||
      typeof entry.kind !== "string" ||
      !AGENT_ACTIVITY_KINDS.has(entry.kind as AgentActivityKind) ||
      typeof entry.outcome !== "string" ||
      !AGENT_ACTIVITY_OUTCOMES.has(entry.outcome as AgentActivityOutcome) ||
      typeof entry.at !== "string" ||
      entry.at.length === 0
    ) {
      return [];
    }
    return [
      {
        kind: entry.kind as AgentActivityKind,
        outcome: entry.outcome as AgentActivityOutcome,
        at: entry.at,
      },
    ];
  });
}

export function parseSandboxAgentCancellation(
  value: unknown,
): SandboxAgentCancellation | null {
  if (!isRecord(value)) return null;
  const keys = Object.keys(value);
  if (
    keys.some((key) => key !== "id" && key !== "status") ||
    typeof value.id !== "string" ||
    value.id.length === 0 ||
    (value.status !== "cancelling" && value.status !== "cancelled")
  ) {
    return null;
  }
  return { id: value.id, status: value.status };
}

/**
 * Every key a pending approval may carry.
 *
 * `satisfies` ties this to the generated wire type, so a field renamed
 * server-side fails to compile here. It used to be eight string literals
 * compared by hand: a rename would have left them allowing the old name and
 * rejecting the new one, so the validator would reject every approval and the
 * consent prompt would simply stop appearing, with nothing failing.
 */
const PENDING_APPROVAL_KEYS = [
  "call_id",
  "turn_id",
  "action",
  "approval",
  "class",
  "preview",
  "can_approve",
  "can_remember",
  "grant_rungs",
  "auto_judge_status",
] as const satisfies readonly (keyof PendingApprovalSnapshot)[];

export function parsePendingToolApproval(
  value: unknown,
): PendingToolApproval | null {
  if (!isRecord(value)) return null;
  const keys = Object.keys(value);
  if (
    keys.some(
      (key) => !(PENDING_APPROVAL_KEYS as readonly string[]).includes(key),
    ) ||
    typeof value.call_id !== "string" ||
    value.call_id.length === 0 ||
    typeof value.turn_id !== "string" ||
    value.turn_id.length === 0 ||
    !isRendererToolName(value.action) ||
    !isRendererApprovalKind(value.approval) ||
    (value.class !== "read_only" &&
      value.class !== "workspace" &&
      value.class !== "sensitive") ||
    typeof value.can_approve !== "boolean" ||
    value.can_approve !== isApprovableKind(value.approval) ||
    typeof value.can_remember !== "boolean" ||
    !Array.isArray(value.grant_rungs) ||
    value.grant_rungs.some((rung) => parseApprovalGrantRung(rung) === null) ||
    (value.grant_rungs.length > 0 && !isRememberableKind(value.approval)) ||
    value.can_remember !== (value.grant_rungs.length > 0) ||
    !(
      value.auto_judge_status === undefined ||
      value.auto_judge_status === "judging" ||
      value.auto_judge_status === "approved" ||
      value.auto_judge_status === "declined"
    )
  ) {
    return null;
  }
  return {
    callId: value.call_id,
    turnId: value.turn_id,
    action: value.action,
    approval: value.approval,
    class: value.class,
    preview: parseToolActionPreview(value.preview),
    canApprove: value.can_approve,
    canRemember: value.can_remember,
    grantRungs: value.grant_rungs.map(
      (rung) => parseApprovalGrantRung(rung) as ApprovalGrantRung,
    ),
    autoJudgeStatus: value.auto_judge_status ?? null,
  };
}

function parseApprovalGrantRung(value: unknown): ApprovalGrantRung | null {
  if (value === "exact_action" || value === "whole_tool") return value;
  if (
    !isRecord(value) ||
    Object.keys(value).length !== 1 ||
    !isRecord(value.command_prefix) ||
    Object.keys(value.command_prefix).length !== 1
  ) {
    return null;
  }
  const tokens = value.command_prefix.tokens;
  return typeof tokens === "number" &&
    Number.isInteger(tokens) &&
    tokens > 0
    ? { command_prefix: { tokens } }
    : null;
}

/**
 * Validate a preview field by field. A malformed or unrecognized preview is
 * dropped rather than partially rendered: an approval card that describes the
 * wrong action is worse than one that describes no action.
 */
export function parseToolActionPreview(
  value: unknown,
): ToolActionPreview | null {
  if (value === undefined || value === null) return null;
  if (!isRecord(value)) return null;
  if (value.tool === "search") {
    const { query } = value;
    if (typeof query !== "string" || query.length === 0) return null;
    return { tool: "search", query };
  }
  if (value.tool === "web_search") {
    const { query, domains, start_published_at, end_published_at } = value;
    if (
      typeof query !== "string" ||
      query.length === 0 ||
      !Array.isArray(domains) ||
      !domains.every((domain): domain is string => typeof domain === "string") ||
      !isOptionalString(start_published_at) ||
      !isOptionalString(end_published_at)
    ) {
      return null;
    }
    return {
      tool: "web_search",
      query,
      domains,
      start_published_at,
      end_published_at,
    };
  }
  if (value.tool === "web_extract") {
    const { url } = value;
    if (typeof url !== "string" || url.length === 0) return null;
    return { tool: "web_extract", url };
  }
  if (value.tool !== "exec") return null;
  const { command, args, cwd, files } = value;
  // `files` joined the projection after previews were already being stored, so
  // an absent list reads as staging nothing rather than dropping the card.
  const staged = files === undefined ? [] : files;
  if (
    typeof command !== "string" ||
    command.length === 0 ||
    !Array.isArray(args) ||
    !args.every((arg): arg is string => typeof arg === "string") ||
    typeof cwd !== "string" ||
    cwd.length === 0 ||
    !Array.isArray(staged) ||
    !staged.every((file): file is string => typeof file === "string")
  ) {
    return null;
  }
  return { tool: "exec", command, args, cwd, files: staged };
}

/**
 * The wire shape this validator reads, with every value still unverified.
 *
 * Keyed on the generated type rather than on `string`, because the destructuring
 * below is the one place the snake_case wire form is written out by hand. If a
 * field is renamed in Rust, the name disappears from `keyof` and this fails to
 * compile — instead of quietly destructuring to `undefined` and dropping every
 * result preview at runtime, which no test would have caught.
 */
type UncheckedExecResult = Partial<
  Record<keyof Extract<WireToolResultPreview, { tool: "exec" }>, unknown>
>;

type UncheckedMcpAppResult = Partial<
  Record<keyof Extract<WireToolResultPreview, { tool: "mcp_app" }>, unknown>
>;

type UncheckedEntriesResult = Partial<
  Record<keyof Extract<WireToolResultPreview, { tool: "entries" }>, unknown>
>;

const RESULT_ENTRY_KINDS: readonly ResultEntryKind[] = [
  "file",
  "folder",
  "source",
  "passage",
  "link",
  "output",
];

/**
 * Validate one listed row.
 *
 * A row is dropped rather than partially rendered, on the same terms as a whole
 * preview: a row with no label is a blank line the reader cannot interpret, and
 * an unrecognized kind would reach the icon map with nothing to draw.
 */
function parseResultEntry(value: unknown): ResultEntry | null {
  if (!isRecord(value)) return null;
  const { kind, label } = value;
  // A missing hint is faithfully the absence the row shows, so `detail` and
  // `meta` are normalized rather than validated — only a present value of the
  // wrong type would be a reason to distrust the row, and it drops that field.
  const detail = value.detail ?? null;
  const meta = value.meta ?? null;
  const mediaType = value.media_type ?? null;
  const outputId = value.output_id ?? null;
  if (
    typeof label !== "string" ||
    label.length === 0 ||
    !(RESULT_ENTRY_KINDS as readonly unknown[]).includes(kind) ||
    !isOptionalString(detail) ||
    !isOptionalString(meta) ||
    !isOptionalString(mediaType) ||
    !isOptionalString(outputId)
  ) {
    return null;
  }
  return { kind: kind as ResultEntryKind, label, detail, meta, mediaType, outputId };
}

/**
 * Validate one failure row.
 *
 * The reason is what the row exists to say, so a row without a readable one is
 * dropped — and, like a dropped entry, counted as not shown rather than
 * vanishing. A failure the card quietly omits is the worst kind of omission.
 */
function parseResultFailure(value: unknown): ResultFailure | null {
  if (!isRecord(value)) return null;
  const { error } = value;
  const label = value.label ?? null;
  if (typeof error !== "string" || error.length === 0 || !isOptionalString(label)) {
    return null;
  }
  return { label, error };
}

/**
 * Validate a result field by field, on the same terms as an action: anything
 * that cannot be fully verified is dropped rather than half-rendered.
 */
export function parseToolResultPreview(
  value: unknown,
): ToolResultPreview | null {
  if (value === undefined || value === null) return null;
  if (!isRecord(value)) return null;
  if (value.tool === "web_search_provider_required") {
    return { tool: "web_search_provider_required" };
  }
  if (value.tool === "mcp_app") {
    const { server, resource_uri }: UncheckedMcpAppResult = value;
    if (
      typeof server !== "string" ||
      server.length === 0 ||
      typeof resource_uri !== "string" ||
      !resource_uri.startsWith("ui://")
    ) {
      return null;
    }
    return { tool: "mcp_app", server, resourceUri: resource_uri };
  }
  if (value.tool === "entries") {
    const { entries, failures, elided }: UncheckedEntriesResult = value;
    if (
      !Array.isArray(entries) ||
      !Array.isArray(failures) ||
      !Number.isInteger(elided) ||
      Number(elided) < 0
    ) {
      return null;
    }
    const parsedEntries = entries
      .map(parseResultEntry)
      .filter((entry): entry is ResultEntry => entry !== null);
    const parsedFailures = failures
      .map(parseResultFailure)
      .filter((failure): failure is ResultFailure => failure !== null);
    // Rows this parser rejected are counted with the ones the server bounded
    // away, because in both cases the card is showing fewer results than the
    // call returned and has to say so.
    return {
      tool: "entries",
      entries: parsedEntries,
      failures: parsedFailures,
      elided:
        Number(elided) +
        (entries.length - parsedEntries.length) +
        (failures.length - parsedFailures.length),
    };
  }
  if (value.tool !== "exec") return null;
  const {
    exit_code,
    timed_out,
    output_truncated,
    stdout,
    stderr,
    images,
    outputs,
    degraded,
    backend,
  }: UncheckedExecResult = value;
  const imageValues = images ?? [];
  const outputValues = outputs ?? [];
  if (!Array.isArray(outputValues)) return null;
  // Like listed entries, a malformed output row is dropped rather than
  // poisoning the whole preview: the rows are display hints, not authority.
  const parsedOutputs = outputValues
    .map(parseResultEntry)
    .filter((entry): entry is ResultEntry => entry !== null);
  if (
    (exit_code !== null && typeof exit_code !== "number") ||
    typeof timed_out !== "boolean" ||
    typeof output_truncated !== "boolean" ||
    typeof stdout !== "string" ||
    typeof stderr !== "string" ||
    !Array.isArray(imageValues)
  ) {
    return null;
  }
  const parsedImages = imageValues
    .map((image) => {
      if (!isRecord(image)) return null;
      const { blob_id, media_type, width, height } = image;
      if (
        typeof blob_id !== "string" ||
        !["png", "jpeg", "webp"].includes(String(media_type)) ||
        !Number.isInteger(width) ||
        Number(width) <= 0 ||
        !Number.isInteger(height) ||
        Number(height) <= 0
      ) {
        return null;
      }
      return {
        attachmentId: blob_id,
        mediaType: media_type === "jpeg" ? "image/jpeg" : `image/${media_type}`,
        width: Number(width),
        height: Number(height),
      };
    })
    .filter((image): image is NonNullable<typeof image> => image !== null);
  if (parsedImages.length !== imageValues.length) return null;
  return {
    tool: "exec",
    exitCode: exit_code,
    timedOut: timed_out,
    outputTruncated: output_truncated,
    stdout,
    stderr,
    images: parsedImages,
    outputs: parsedOutputs,
    // Unknown to this build means unshowable, not unusable: the command's
    // output still renders, without a sentence nobody wrote copy for.
    degraded: isExecDegradation(degraded) ? degraded : undefined,
    backend: isExecBackend(backend) ? backend : undefined,
  };
}

function isExecDegradation(value: unknown): value is ExecDegradation {
  return value === "sandbox_image_unavailable";
}

function isExecBackend(value: unknown): value is ExecBackend {
  return value === "local" || value === "e2b" || value === "daytona";
}

/**
 * Whether a provider-supplied string is a tool name the renderer will accept.
 *
 * Still an allowlist, and still a closed one — the difference is that the list
 * is now the server's own enum rather than a copy of it maintained here. The
 * copy drifted three times: two tools reached the union with no icon, and one
 * had no historical title, so a command relabelled itself on reload.
 */
export function isRendererToolName(value: unknown): value is RendererToolName {
  return (
    typeof value === "string" &&
    (RENDERER_TOOL_NAMES as readonly string[]).includes(value)
  );
}

function isRendererApprovalKind(value: unknown): value is RendererApprovalKind {
  return (
    value === "search_may_share_query_and_excerpts" ||
    value === "web_search_may_share_query" ||
    value === "web_extract_may_fetch_url" ||
    value === "exec_may_run_networked_command" ||
    value === "external_mcp_may_call_server" ||
    value === "workspace_may_modify_files" ||
    value === "unsupported"
  );
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/**
 * A field the server sends as `null` when the model did not set it.
 *
 * `undefined` is not accepted: a missing key on this surface means the payload
 * is not the shape it claims to be, which is what the validator is for.
 */
function isOptionalString(value: unknown): value is string | null {
  return value === null || (typeof value === "string" && value.length > 0);
}
