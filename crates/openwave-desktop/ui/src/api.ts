import {
  RENDERER_TOOL_NAMES,
  type ApprovalClass,
  type RendererAgentEvent,
  type RendererSequencedEvent,
  type RendererToolName,
  type ToolActionPreview,
  type ToolApprovalKind,
  type ToolResultPreview as WireToolResultPreview,
} from "./generated/wire";

export type { ApprovalClass, RendererToolName, ToolActionPreview };

/**
 * The WebSocket frame and the events it carries, generated from the server's
 * renderer projection.
 *
 * Named for the renderer on the Rust side; the shorter names are what the app
 * has always used, so they are aliased rather than renamed everywhere.
 */
export type SequencedEvent = RendererSequencedEvent;
export type AgentEvent = RendererAgentEvent;

/** Generated from `ToolApprovalKind`. */
export type RendererApprovalKind = ToolApprovalKind;

/** Connection details from the Tauri host (`server_info` command). */
export type ServerInfo = {
  baseUrl: string;
  token: string;
};

export type ProviderKind = "anthropic" | "openai" | "openai_compatible";
/** Stable provider-scoped key used for new settings and chat overrides. */
export type ModelSelectionKey = `${ProviderKind}::${string}`;

/** How hard a reasoning-capable model should think before answering. */
export type ReasoningEffort = "low" | "medium" | "high";

export type ProviderInfo = {
  kind: ProviderKind;
  enabled: boolean;
  has_credential: boolean;
  /** Absent, not null, when unset — the server skips serializing `None`. */
  base_url?: string;
  models: CustomModelConfig[];
};

export type CustomModelConfig = {
  id: string;
  /** Absent, not null, when unset — the server skips serializing `None`. */
  display_name?: string;
  context_window: number;
  max_output_tokens: number;
};

export type ModelInfo = {
  /** Stable provider-qualified selection key. */
  key: ModelSelectionKey;
  id: string;
  /** Human-readable label for the selector (e.g. "Claude Opus 4.8"). */
  display_name: string;
  provider: ProviderKind;
  /** Provider is enabled, configured, and credentialed. */
  available: boolean;
  /** Approximate context window in tokens. */
  context_window: number;
  /** Maximum response size in tokens. */
  max_output_tokens: number;
  /** Input modalities accepted by the model. */
  input_modalities: Array<"text" | "image">;
  /** Whether the model can produce an internal reasoning stream. */
  supports_reasoning: boolean;
  /** Whether the model exposes a reasoning-effort control. */
  supports_reasoning_effort: boolean;
  /** Whether the model accepts image input alongside text. */
  multimodal: boolean;
};

/** Global runtime settings (`GET/PUT /settings`). */
export type RuntimeSettings = {
  /** The default model, or `null` when the server default is in effect. */
  model: string | null;
  has_api_key: boolean;
};

export type Project = {
  id: string;
  title: string | null;
  attachment_revision: number;
  root_attachments: string[];
  created_at: string;
};

/** The fixed, host-owned search providers supported by this build. */
export type WebSearchProviderKind = "exa" | "tavily";

/** Non-secret web-search policy and readiness for its selected provider. */
export type WebSearchConfigInfo = {
  provider?: WebSearchProviderKind;
  timeout_ms: number;
  has_credential: boolean;
};

/** Readiness only: the API never returns an existing provider key. */
export type WebSearchCredentialReadiness = {
  provider: WebSearchProviderKind;
  has_credential: boolean;
};

/** The fixed, host-owned code-execution providers supported by this build. */
export type CodeExecutionProviderKind = "local";

/** Non-secret code-execution selection, timeout policy, and host readiness. */
export type CodeExecutionConfigInfo = {
  provider?: CodeExecutionProviderKind;
  timeout_ms: number;
  available: boolean;
};

export type McpHealth =
  | "initializing"
  | "healthy"
  | "degraded"
  | "reconnecting"
  | "disabled";

/** Typed stdio process data. Values are argv entries, never shell source. */
export type McpServerDefinition = {
  name: string;
  command: string;
  args: string[];
  /** Literal values must be non-secret; credentials use `env_from` names. */
  env: Record<string, string>;
  env_from: string[];
  cwd: string | null;
  request_timeout_ms: number;
  enabled: boolean;
};

/** Renderer-safe health projection. Resolved `env_from` values are never sent. */
export type McpServerInfo = McpServerDefinition & {
  health: McpHealth;
  tool_count: number;
  diagnostic: string | null;
};

export type McpServersInfo = {
  servers: McpServerInfo[];
};

export type Chat = {
  id: string;
  title: string | null;
  model: string | null;
  /** Reasoning-effort override, or `null` to use the provider default. */
  reasoning_effort: ReasoningEffort | null;
  attachment_revision: number;
  root_attachments: Array<{
    root_id: string;
    origin: "project_default" | "conversation";
  }>;
  project_id: string | null;
  created_at: string;
};

/** One visible, durable transcript entry in conversation order. */
export type ChatMessage = {
  id: string;
  role: "user" | "assistant";
  content: string;
  created_at: string;
  /**
   * Deliberately wider than the wire: the server always serializes this, as
   * `[]` when empty. It is optional here because the transcript is not run
   * through a validator — it arrives as a parsed cast — so the `?` is what
   * makes the compiler demand a guard at the one place that reads it. Narrowing
   * this to match the wire would delete that guard, not earn it.
   */
  citations?: ChatMessageCitation[];
};

/**
 * A bounded, renderer-safe evidence snapshot owned by one assistant message.
 *
 * Ownership is positional: the server nests each citation under its message and
 * deliberately skips `message_id` on the wire. Do not reintroduce it.
 */
export type ChatMessageCitation = {
  id: string;
  ordinal: number;
  excerpt: string;
  heading: string | null;
  pages: number[];
};

/** A fixed, terminal tool-card projection with no canonical tool data. */
export type ChatToolActivity = {
  /** What the call did, when its tool projects it. */
  action?: ToolActionPreview;
  /**
   * Allowlisted renderer tool name, folded server-side.
   *
   * A name rather than display copy: the renderer derives a live call's
   * wording from its name, and carrying prose here meant a second copy of it
   * plus an inverse lookup, where a change on either side silently broke
   * hydration.
   */
  tool: RendererToolName;
  status: "completed" | "failed" | "cancelled";
  started_at: string;
  finished_at: string | null;
};

export type ChatTranscript = {
  messages: ChatMessage[];
  tool_activity: ChatToolActivity[];
  last_event_seq: number;
};

/** A durable foreground coordinator or sandboxed background run. */
export type AgentRun = {
  id: string;
  parent_id: string | null;
  execution: "foreground" | "sandbox";
  status:
    | "active"
    | "queued"
    | "running"
    | "cancelling"
    | "waiting"
    | "retry_wait"
    | "completed"
    | "failed"
    | "cancelled";
  started_at: string | null;
  finished_at: string | null;
  last_error_code: string | null;
  /** A fixed, renderer-safe description of the currently live agent task. */
  activity: AgentActivity | null;
  created_at: string;
  updated_at: string;
};

/** The complete renderer projection returned by one sandbox stop request. */
export type SandboxAgentCancellation = {
  id: string;
  status: "cancelling" | "cancelled";
};

/**
 * Live agent activity is intentionally a closed vocabulary. The server never
 * sends tool inputs, results, host paths, grants, executor identities, leases,
 * provider identities, or diagnostics.
 */
export type AgentActivity = {
  kind:
    | "web_search"
    | "read_delegated_file"
    | "list_connected_folders"
    | "list_folder"
    | "read_connected_file"
    | "import_connected_file";
  status: "waiting" | "running";
};



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
export type ToolResultPreview = {
  tool: "exec";
  exitCode: number | null;
  timedOut: boolean;
  outputTruncated: boolean;
  stdout: string;
  stderr: string;
};


/** Approval kinds a human may approve from the renderer. */
export function isApprovableKind(kind: RendererApprovalKind): boolean {
  return (
    kind === "search_may_share_query_and_excerpts" ||
    kind === "web_search_may_share_query" ||
    kind === "exec_may_run_networked_command" ||
    kind === "external_mcp_may_call_server"
  );
}

/**
 * How wide a standing grant to remember, narrowest first.
 *
 * The renderer only names the rung. The server builds the concrete grant from
 * the arguments the call is parked on, so a grant can never describe a broader
 * action than the one the human was shown.
 */
export type ApprovalGrantRung =
  | "exact_command"
  | "any_args_for_command"
  | "whole_tool";

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
  optionId?: string;
  freeForm?: string;
};

const RENDERER_FOLDER_ACCESS_REASON =
  "The assistant needs read access to files outside the folders connected to this conversation.";

const WS_HANDSHAKE = "openwave-v1";
const WS_TOKEN_PREFIX = "openwave-token.";

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
    if (!response.ok) {
      let detail = response.statusText;
      try {
        const body = (await response.json()) as { message?: string };
        if (body.message) detail = body.message;
      } catch {
        /* ignore */
      }
      throw new Error(`${response.status}: ${detail}`);
    }
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
      credential?: { type: "api_key"; key: string };
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

  listModels(): Promise<{ models: ModelInfo[] }> {
    return this.json("/models", { headers: this.headers() });
  }

  getSettings(): Promise<RuntimeSettings> {
    return this.json("/settings", { headers: this.headers() });
  }

  /**
   * Update runtime settings. `model` absent leaves it unchanged, `null` resets
   * it to the server default, and a value sets it (matching the double-option
   * body the server expects).
   */
  putSettings(body: { model?: ModelSelectionKey | null }): Promise<RuntimeSettings> {
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
  }): Promise<CodeExecutionConfigInfo> {
    return this.json("/code-execution", {
      method: "PUT",
      headers: this.headers(true),
      body: JSON.stringify(body),
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

  createChat(model?: ModelSelectionKey, projectId?: string | null): Promise<Chat> {
    return this.json("/chats", {
      method: "POST",
      headers: this.headers(true),
      body: JSON.stringify({
        model: model || undefined,
        project_id: projectId || undefined,
      }),
    });
  }

  listChats(): Promise<Chat[]> {
    return this.json("/chats", { headers: this.headers() });
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

  listAgentRuns(chatId: string): Promise<AgentRun[]> {
    return this.json(`/chats/${chatId}/agent-runs`, {
      headers: this.headers(),
    });
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

  postMessage(chatId: string, turnId: string, content: string): Promise<void> {
    return this.json(`/chats/${chatId}/messages`, {
      method: "POST",
      headers: this.headers(true),
      body: JSON.stringify({ turn_id: turnId, content }),
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
  ): Promise<void> {
    await this.json(`/chats/${chatId}/questions/${callId}/answer`, {
      method: "POST",
      headers: this.headers(true),
      body: JSON.stringify({
        answers: answers.map((answer) => ({
          question_id: answer.questionId,
          ...(answer.optionId === undefined
            ? {}
            : { option_id: answer.optionId }),
          ...(answer.freeForm === undefined
            ? {}
            : { free_form: answer.freeForm }),
        })),
      }),
    });
  }

  /** Open the chat event stream; auth via Sec-WebSocket-Protocol. */
  openEvents(chatId: string, after: number, onEvent: (e: SequencedEvent) => void): WebSocket {
    const url = `${this.baseUrl.replace(/^http/, "ws")}/chats/${chatId}/events?after=${after}`;
    const protocols = [WS_HANDSHAKE, `${WS_TOKEN_PREFIX}${this.token}`];
    const socket = new WebSocket(url, protocols);
    socket.onmessage = (msg) => {
      try {
        onEvent(JSON.parse(String(msg.data)) as SequencedEvent);
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
  const keys = Object.keys(value);
  if (
    keys.some(
      (key) =>
        key !== "call_id" &&
        key !== "turn_id" &&
        key !== "reason" &&
        key !== "folder_hint" &&
        key !== "claimed",
    ) ||
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

export function parsePendingUserQuestions(
  value: unknown,
): PendingUserQuestions | null {
  if (
    !isRecord(value) ||
    !onlyKeys(value, ["call_id", "turn_id", "questions", "asked_at"]) ||
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
      !onlyKeys(item, [
        "id",
        "header",
        "question",
        "options",
        "allow_free_form",
      ]) ||
      !nonEmptyBounded(item.id, 64) ||
      questionIds.has(item.id) ||
      !nonEmptyBounded(item.header, 32) ||
      !nonEmptyBounded(item.question, 500) ||
      !Array.isArray(item.options) ||
      item.options.length > 5 ||
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
        !onlyKeys(option, ["id", "label", "description"]) ||
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

function onlyKeys(value: Record<string, unknown>, allowed: string[]): boolean {
  const set = new Set(allowed);
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

export function parsePendingToolApproval(
  value: unknown,
): PendingToolApproval | null {
  if (!isRecord(value)) return null;
  const keys = Object.keys(value);
  if (
    keys.some(
      (key) =>
        key !== "call_id" &&
        key !== "turn_id" &&
        key !== "action" &&
        key !== "approval" &&
        key !== "class" &&
        key !== "preview" &&
        key !== "can_approve" &&
        key !== "can_remember",
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
    value.can_remember !== isRememberableKind(value.approval)
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
  };
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
  if (value.tool === "search" || value.tool === "web_search") {
    const { query } = value;
    if (typeof query !== "string" || query.length === 0) return null;
    return { tool: value.tool, query };
  }
  if (value.tool !== "exec") return null;
  const { command, args, cwd } = value;
  if (
    typeof command !== "string" ||
    command.length === 0 ||
    !Array.isArray(args) ||
    !args.every((arg): arg is string => typeof arg === "string") ||
    typeof cwd !== "string" ||
    cwd.length === 0
  ) {
    return null;
  }
  return { tool: "exec", command, args, cwd };
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

/**
 * Validate a result field by field, on the same terms as an action: anything
 * that cannot be fully verified is dropped rather than half-rendered.
 */
export function parseToolResultPreview(
  value: unknown,
): ToolResultPreview | null {
  if (value === undefined || value === null) return null;
  if (!isRecord(value) || value.tool !== "exec") return null;
  const { exit_code, timed_out, output_truncated, stdout, stderr }: UncheckedExecResult = value;
  if (
    (exit_code !== null && typeof exit_code !== "number") ||
    typeof timed_out !== "boolean" ||
    typeof output_truncated !== "boolean" ||
    typeof stdout !== "string" ||
    typeof stderr !== "string"
  ) {
    return null;
  }
  return {
    tool: "exec",
    exitCode: exit_code,
    timedOut: timed_out,
    outputTruncated: output_truncated,
    stdout,
    stderr,
  };
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
    value === "exec_may_run_networked_command" ||
    value === "external_mcp_may_call_server" ||
    value === "unsupported"
  );
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
