/** Connection details from the Tauri host (`server_info` command). */
export type ServerInfo = {
  baseUrl: string;
  token: string;
};

export type ProviderKind = "anthropic" | "openai" | "openai_compatible";

/** How hard a reasoning-capable model should think before answering. */
export type ReasoningEffort = "low" | "medium" | "high";

export type ProviderInfo = {
  kind: ProviderKind;
  enabled: boolean;
  has_credential: boolean;
  base_url: string | null;
};

export type ModelInfo = {
  id: string;
  /** Human-readable label for the selector (e.g. "Claude Opus 4.8"). */
  display_name: string;
  provider: string;
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
  /** Optional so a partial renderer-boundary response remains non-fatal. */
  citations?: ChatMessageCitation[];
};

/** A bounded, renderer-safe evidence snapshot owned by one assistant message. */
export type ChatMessageCitation = {
  id: string;
  message_id: string;
  ordinal: number;
  excerpt: string;
  heading: string | null;
  pages: number[];
};

/** A fixed, terminal tool-card projection with no canonical tool data. */
export type ChatToolActivity = {
  title:
    | "Search sources"
    | "Check sources"
    | "Read a source"
    | "Search the web"
    | "Read a delegated file"
    | "Read a file"
    | "Browse files"
    | "Update a file"
    | "Request folder access"
    | "Connect a folder"
    | "Check connected folders"
    | "Delegate a task"
    | "Wait for background agents"
    | "Use a tool";
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
    | "read_connected_file";
  status: "waiting" | "running";
};

export type SequencedEvent = {
  seq: number;
  event: AgentEvent;
};

export type AgentEvent =
  | { type: "turn_started"; turn_id: string }
  | { type: "text_delta"; text: string }
  | { type: "reasoning_delta" }
  | { type: "stream_interrupted" }
  | { type: "tool_call_started"; call_id: string; name: RendererToolName }
  | { type: "tool_call_args_delta"; call_id: string }
  | {
      type: "approval_required";
      call_id: string;
      action: RendererToolName;
      approval: RendererApprovalKind;
      class: "read_only" | "workspace" | "sensitive";
    }
  | { type: "approval_decided"; call_id: string; approved: boolean }
  | {
      type: "tool_call_completed";
      call_id: string;
      status: "completed" | "failed";
    }
  | { type: "turn_completed" }
  | { type: "turn_failed" }
  | { type: "turn_cancelled" }
  | { type: "user_steered"; message_id: string; text: string }
  | { type: "context_truncated" }
  | { type: "event_omitted" };

export type RendererToolName =
  | "search"
  | "list_sources"
  | "read_source"
  | "web_search"
  | "read_delegated_file"
  | "read_file"
  | "list_dir"
  | "write_file"
  | "request_folder_access"
  | "connect_folder"
  | "list_connected_folders"
  | "list_folder"
  | "read_connected_file"
  | "spawn_sandbox_agent"
  | "wait_for_agents"
  | "exec"
  | "other";

export type RendererApprovalKind =
  | "search_may_share_query_and_excerpts"
  | "web_search_may_share_query"
  | "exec_may_run_networked_command"
  | "unsupported";

/** Approval kinds a human may approve from the renderer. */
export function isApprovableKind(kind: RendererApprovalKind): boolean {
  return (
    kind === "search_may_share_query_and_excerpts" ||
    kind === "web_search_may_share_query" ||
    kind === "exec_may_run_networked_command"
  );
}

/** A strict renderer-safe snapshot used to recover a parked approval. */
export type PendingToolApproval = {
  callId: string;
  turnId: string;
  action: RendererToolName;
  approval: RendererApprovalKind;
  class: "read_only" | "workspace" | "sensitive";
  canApprove: boolean;
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
  putSettings(body: { model?: string | null }): Promise<RuntimeSettings> {
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

  createChat(model?: string, projectId?: string | null): Promise<Chat> {
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

  patchChatModel(chatId: string, model: string | null): Promise<Chat> {
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
    remember = false,
  ): Promise<void> {
    return this.json(`/chats/${chatId}/approvals/${callId}`, {
      method: "POST",
      headers: this.headers(true),
      body: JSON.stringify({ decision, remember }),
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
        key !== "can_approve",
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
    value.can_approve !== isApprovableKind(value.approval)
  ) {
    return null;
  }
  return {
    callId: value.call_id,
    turnId: value.turn_id,
    action: value.action,
    approval: value.approval,
    class: value.class,
    canApprove: value.can_approve,
  };
}

function isRendererToolName(value: unknown): value is RendererToolName {
  return (
    value === "search" ||
    value === "list_sources" ||
    value === "read_source" ||
    value === "web_search" ||
    value === "read_delegated_file" ||
    value === "read_file" ||
    value === "list_dir" ||
    value === "write_file" ||
    value === "request_folder_access" ||
    value === "connect_folder" ||
    value === "list_connected_folders" ||
    value === "list_folder" ||
    value === "read_connected_file" ||
    value === "spawn_sandbox_agent" ||
    value === "wait_for_agents" ||
    value === "exec" ||
    value === "other"
  );
}

function isRendererApprovalKind(value: unknown): value is RendererApprovalKind {
  return (
    value === "search_may_share_query_and_excerpts" ||
    value === "web_search_may_share_query" ||
    value === "exec_may_run_networked_command" ||
    value === "unsupported"
  );
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
