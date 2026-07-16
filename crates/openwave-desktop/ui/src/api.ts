/** Connection details from the Tauri host (`server_info` command). */
export type ServerInfo = {
  baseUrl: string;
  token: string;
};

export type ProviderKind = "anthropic" | "openai" | "openai_compatible";

export type ProviderInfo = {
  kind: ProviderKind;
  enabled: boolean;
  has_credential: boolean;
  base_url: string | null;
};

export type ModelInfo = {
  id: string;
  provider: string;
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

export type Chat = {
  id: string;
  title: string | null;
  model: string | null;
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
};

/** A fixed, terminal tool-card projection with no canonical tool data. */
export type ChatToolActivity = {
  title:
    | "Search the web"
    | "Read a file"
    | "Browse files"
    | "Update a file"
    | "Request folder access"
    | "Connect a folder"
    | "Check connected folders"
    | "Delegate a task"
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

/**
 * Live agent activity is intentionally a closed vocabulary. The server never
 * sends tool inputs, results, host paths, grants, executor identities, leases,
 * provider identities, or diagnostics.
 */
export type AgentActivity = {
  kind:
    | "web_search"
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
  | { type: "reasoning_delta"; text: string }
  | { type: "stream_interrupted" }
  | { type: "tool_call_started"; call_id: string; name: string }
  | { type: "tool_call_args_delta"; call_id: string; fragment: string }
  | {
      type: "approval_required";
      call_id: string;
      class: string;
      summary: string;
    }
  | { type: "approval_decided"; call_id: string; approved: boolean }
  | { type: "tool_call_completed"; call_id: string; output: unknown }
  | { type: "turn_completed"; usage: unknown; stop_reason: string }
  | { type: "turn_failed"; error: { kind: string; message: string } }
  | { type: "turn_cancelled"; usage: unknown }
  | { type: "user_steered"; message_id: string; content: string };

export type FolderAccessHint = "documents" | "downloads";

/** A validated, pending request that the renderer may safely present. */
export type PendingFolderAccessRequest = {
  callId: string;
  turnId: string;
  reason: string;
  folderHint: FolderAccessHint | null;
  claimedByDesktop: boolean;
};

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

  private async json<T>(path: string, init?: RequestInit): Promise<T> {
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
    if (response.status === 204 || response.status === 202) return undefined as T;
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

  createChat(model?: string): Promise<Chat> {
    return this.json("/chats", {
      method: "POST",
      headers: this.headers(true),
      body: JSON.stringify({ model: model || undefined }),
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

  listAgentRuns(chatId: string): Promise<AgentRun[]> {
    return this.json(`/chats/${chatId}/agent-runs`, {
      headers: this.headers(),
    });
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
  ): Promise<void> {
    return this.json(`/chats/${chatId}/approvals/${callId}`, {
      method: "POST",
      headers: this.headers(true),
      body: JSON.stringify({ decision }),
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
      const request = parseFolderAccessRequest(item, chatId);
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

function parseFolderAccessRequest(
  value: unknown,
  chatId: string,
): PendingFolderAccessRequest | null {
  if (!isRecord(value)) return null;
  if (
    typeof value.id !== "string" ||
    value.id.length === 0 ||
    typeof value.turn_id !== "string" ||
    value.turn_id.length === 0 ||
    value.chat_id !== chatId ||
    value.name !== "request_folder_access" ||
    value.execution !== "client" ||
    value.status !== "pending" ||
    !(value.client_executor_id === null ||
      typeof value.client_executor_id === "string")
  ) {
    return null;
  }

  const args = value.arguments;
  if (!isRecord(args)) return null;
  const keys = Object.keys(args);
  if (
    keys.some(
      (key) =>
        key !== "reason" &&
        key !== "requested_capabilities" &&
        key !== "folder_hint",
    ) ||
    typeof args.reason !== "string" ||
    args.reason.trim().length === 0 ||
    [...args.reason].length > 500 ||
    args.reason.includes("\0") ||
    !Array.isArray(args.requested_capabilities) ||
    args.requested_capabilities.length !== 1 ||
    args.requested_capabilities[0] !== "read_files"
  ) {
    return null;
  }

  const folderHint = args.folder_hint;
  if (
    folderHint !== undefined &&
    folderHint !== "documents" &&
    folderHint !== "downloads"
  ) {
    return null;
  }

  return {
    callId: value.id,
    turnId: value.turn_id,
    reason: args.reason,
    folderHint: folderHint ?? null,
    claimedByDesktop: value.client_executor_id !== null,
  };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
