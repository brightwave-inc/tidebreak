/** Connection details from the Tauri host (`server_info` command). */
export type ServerInfo = {
  baseUrl: string;
  token: string;
  workspaceDir: string;
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

export type Chat = {
  id: string;
  title: string | null;
  model: string | null;
  workspace_dir: string;
  project_id: string | null;
  created_at: string;
};

export type SequencedEvent = {
  seq: number;
  event: AgentEvent;
};

export type AgentEvent =
  | { type: "turn_started"; turn_id: string }
  | { type: "text_delta"; text: string }
  | { type: "reasoning_delta"; text: string }
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
  | { type: "turn_failed"; error: { kind: string; message: string } };

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
    if (response.status === 204) return undefined as T;
    return (await response.json()) as T;
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

  createChat(workspaceDir: string, model?: string): Promise<Chat> {
    return this.json("/chats", {
      method: "POST",
      headers: this.headers(true),
      body: JSON.stringify({
        workspace_dir: workspaceDir,
        model: model || undefined,
      }),
    });
  }

  patchChatModel(chatId: string, model: string | null): Promise<Chat> {
    return this.json(`/chats/${chatId}`, {
      method: "PATCH",
      headers: this.headers(true),
      body: JSON.stringify({ model }),
    });
  }

  postMessage(chatId: string, content: string): Promise<void> {
    return this.json(`/chats/${chatId}/messages`, {
      method: "POST",
      headers: this.headers(true),
      body: JSON.stringify({ content }),
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
