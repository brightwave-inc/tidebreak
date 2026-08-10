import {
  APP_INVOKE_REFUSAL_KINDS,
  AppInvokeRefusalError,
  type AppDetail,
  type AppFolderInvokeResult,
  type AppGrantState,
  type AppInvokeRefusalKind,
  type AppLibrary,
  type AppRestInvokeResult,
  type AppViewSession,
  type ApprovalGrantRung,
  type Chat,
  type ChatFrame,
  type ChatTranscript,
  type CodeExecutionConfigInfo,
  type CodeExecutionCredentialReadiness,
  type CodeExecutionProviderKind,
  type ConnectedAppsInfo,
  type ConsentStatementSnapshot,
  type CustomModelConfig,
  type DocumentDetail,
  type EgressConfig,
  type ExecFileUndoOutcome,
  type FileDownloadProgress,
  type GatewayApps,
  type GatewayStatus,
  type ManagedPolicy,
  type McpAppPayload,
  type McpServerDefinition,
  type McpServersInfo,
  type McpViewSession,
  type ModelCatalog,
  type ModelRole,
  type ModelRoleInfo,
  type ModelSelectionKey,
  type ModelVisibility,
  type NetworkPolicy,
  type PendingFolderAccessRequest,
  type PendingOutputWritebackRequest,
  type PendingPlanApproval,
  type PendingToolApproval,
  type PendingUserQuestions,
  type PermissionMode,
  type PlanDecision,
  type PluginCatalog,
  type PluginEnableUpdate,
  type Project,
  type ProjectDocumentPage,
  type PromptBody,
  type ProviderInfo,
  type ProviderKind,
  type ChatGptSignInStatus,
  type ReasoningEffort,
  type RestCredentialUpdate,
  type RuntimeSettings,
  type SandboxAgentCancellation,
  type SkillInstructions,
  type SpecPreviewInfo,
  type StandingGrantSnapshot,
  type UserQuestionAnswer,
  type VoiceTranscriptionInfo,
  type VoiceTranscriptionModel,
  type WebSearchConfigInfo,
  type WebSearchCredentialReadiness,
  type WebSearchMode,
  type WebSearchProviderKind,
  type AgentActivityHistoryEntry,
  type AgentRun,
  type AgentRunProgress,
  type LocalVoiceInfo,
  type InboxItem,
  type AgentRunTaskPlan,
  type PendingChatPrompt,
  type TaskPlan,
} from "./types";
import {
  parseAgentActivityHistory,
  parseAgentRunProgress,
  parseFolderAccessRequest,
  parseInboxItem,
  parseOutputWritebackRequest,
  parsePendingChatPrompt,
  parsePendingPlanApproval,
  parsePendingToolApproval,
  parsePendingUserQuestions,
  parseAgentRunTaskPlan,
  parseSandboxAgentCancellation,
  parseTaskPlan,
} from "./parsers";

const WS_HANDSHAKE = "openwave-v1";
const WS_TOKEN_PREFIX = "openwave-token.";

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
    if (text.length === 0) return undefined as T;
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
      aws_region?: string | null;
      credential?:
        | { type: "api_key"; key: string }
        | { type: "service_account"; json: string }
        | {
            type: "aws_credentials";
            access_key_id: string;
            secret_access_key: string;
            session_token?: string;
          };
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

  openaiChatgptSignIn(): Promise<{ authorization_url: string }> {
    return this.json("/providers/openai/chatgpt/sign-in", {
      method: "POST",
      headers: this.headers(),
    });
  }

  openaiChatgptSignOut(): Promise<void> {
    return this.json("/providers/openai/chatgpt/sign-out", {
      method: "POST",
      headers: this.headers(),
    });
  }

  getOpenaiChatgptStatus(): Promise<ChatGptSignInStatus> {
    return this.json("/providers/openai/chatgpt/status", {
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
   *
   * `model_visibility_overrides` is the exception to "absent leaves it
   * unchanged, present merges": the server replaces the map wholesale, so a
   * writer sends the complete set of deviations it wants persisted.
   */
  putSettings(body: {
    model?: ModelSelectionKey | null;
    max_active_background_agents?: number;
    model_visibility_overrides?: Record<string, ModelVisibility>;
    compaction?: {
      threshold_fraction?: number;
      target_fraction?: number;
      min_threshold_tokens?: number;
      protect_recent_messages?: number;
    };
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
    mode?: WebSearchMode;
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

  listPlugins(): Promise<PluginCatalog> {
    return this.json("/plugins", { headers: this.headers() });
  }

  /** One skill's full instruction body — what the model is taught by it. */
  getSkillInstructions(name: string): Promise<SkillInstructions> {
    return this.json(`/plugins/skills/${encodeURIComponent(name)}/instructions`, {
      headers: this.headers(),
    });
  }

  /**
   * One prompt's insertable text — what a picker drops into the composer.
   *
   * Its own route because the catalog is read far more often than any single
   * prompt is inserted.
   */
  getPromptBody(name: string): Promise<PromptBody> {
    return this.json(`/plugins/prompts/${encodeURIComponent(name)}/body`, {
      headers: this.headers(),
    });
  }

  /**
   * Set the named enable flags, and take the fresh catalog back.
   *
   * A merge patch: the body names only what is changing, and the response is
   * the authority on what the whole catalog now looks like — which is what
   * lets a surface toggle optimistically and reconcile from one round trip.
   */
  setPluginsEnabled(update: PluginEnableUpdate): Promise<PluginCatalog> {
    return this.json("/plugins/enabled", {
      method: "PUT",
      headers: this.headers(true),
      body: JSON.stringify(update),
    });
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
   * Execute one of an app's pinned REST operations outside any turn.
   * `parameters`, `body`, and the result are opaque passthrough between the
   * sandboxed frame and the server; a typed refusal surfaces as
   * {@link AppInvokeRefusalError} so the caller can branch on
   * `consent_required` without string-matching prose. The response body
   * crosses base64-encoded in `body_base64` (see {@link AppRestInvokeResult}).
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

  /**
   * Execute one folder operation of an app's granted folder binding — the
   * `folder` sibling of {@link invokeAppOperation}, with the same refusal
   * contract. File content crosses base64-encoded in both directions;
   * failures come back as `is_error` results in the host's closed
   * vocabulary.
   */
  async invokeAppFolder(
    appId: string,
    folder: string,
    op: "list" | "read" | "write",
    path?: string,
    contentBase64?: string,
    replace?: boolean,
  ): Promise<AppFolderInvokeResult> {
    const request: Record<string, unknown> = { folder, op };
    if (path !== undefined) request.path = path;
    if (contentBase64 !== undefined) request.content_base64 = contentBase64;
    if (replace !== undefined) request.replace = replace;
    return (await this.postAppInvoke(appId, request)) as AppFolderInvokeResult;
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

  /** The files this project shares with every conversation filed under it. */
  listProjectDocuments(projectId: string): Promise<ProjectDocumentPage> {
    return this.json(`/projects/${encodeURIComponent(projectId)}/documents`, {
      headers: this.headers(),
    });
  }

  /**
   * Share one conversation's file with the project that conversation belongs to.
   *
   * The conversation keeps its own copy — a document's owner is part of its id,
   * so the project's is a different document and the transcript that referred to
   * the original still resolves.
   */
  promoteDocumentToProject(
    projectId: string,
    chatId: string,
    documentId: string,
  ): Promise<{ document_id: string }> {
    return this.json(
      `/projects/${encodeURIComponent(projectId)}/documents/promote`,
      {
        method: "POST",
        headers: this.headers(true),
        body: JSON.stringify({ chat_id: chatId, document_id: documentId }),
      },
    );
  }

  deleteProjectDocument(projectId: string, documentId: string): Promise<void> {
    return this.json(
      `/projects/${encodeURIComponent(projectId)}/documents/${encodeURIComponent(documentId)}`,
      { method: "DELETE", headers: this.headers() },
    );
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

  /**
   * File a chat under a project, or take it back out with `null`.
   *
   * The server refuses (409) a chat that still holds connected folders: its
   * folder grants are keyed to the identity it would be leaving.
   */
  moveChatToProject(chatId: string, projectId: string | null): Promise<Chat> {
    return this.json(`/chats/${encodeURIComponent(chatId)}`, {
      method: "PATCH",
      headers: this.headers(true),
      body: JSON.stringify({ project_id: projectId }),
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

  /**
   * The conversation's current task plan, or `null` when it has none.
   *
   * The journal only carries a hint that the plan moved on, so this is where
   * the steps come from — on the hint, and again on reload. The payload is
   * model-authored text, so it is validated here rather than trusted.
   */
  async getTaskPlan(chatId: string): Promise<TaskPlan | null> {
    const body = await this.json<unknown>(
      `/chats/${encodeURIComponent(chatId)}/task-plan`,
      { headers: this.headers() },
    );
    return parseTaskPlan(body);
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

  /**
   * The full ordered checklist one background run keeps, or `null`.
   *
   * The run snapshot already carries the count and the current step, which is
   * all a status row needs; this is the list behind it, read when a reader
   * opens the run. A wrong-chat, foreground, or missing run answers `404`,
   * which surfaces as a thrown error.
   */
  async getAgentRunTaskPlan(
    chatId: string,
    runId: string,
  ): Promise<AgentRunTaskPlan | null> {
    const body = await this.json<unknown>(
      `/chats/${encodeURIComponent(chatId)}/agent-runs/${encodeURIComponent(runId)}/task-plan`,
      { headers: this.headers() },
    );
    return parseAgentRunTaskPlan(body);
  }

  /**
   * One resumable page of a background run's live progress.
   *
   * Poll with the previous page's `nextSequence` to receive only what the run
   * has published since. A wrong-chat, foreground, or missing run answers
   * `404`, which surfaces as a thrown error.
   */
  async listAgentRunProgress(
    chatId: string,
    runId: string,
    afterSequence = 0,
    limit?: number,
  ): Promise<AgentRunProgress> {
    const query = new URLSearchParams({
      after_sequence: String(afterSequence),
    });
    if (limit !== undefined) query.set("limit", String(limit));
    const body = await this.json<unknown>(
      `/chats/${encodeURIComponent(chatId)}/agent-runs/${encodeURIComponent(runId)}/progress?${query}`,
      { headers: this.headers() },
    );
    return parseAgentRunProgress(body, afterSequence);
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
   *
   * `invokedSkills` names the skills the reader explicitly reached for. A name
   * the install cannot run refuses the whole turn rather than being dropped, so
   * the caller must be ready to show the refusal.
   */
  postMessage(
    chatId: string,
    turnId: string,
    content: string,
    attachments: readonly string[] = [],
    fileAttachments: readonly string[] = [],
    invokedSkills: readonly string[] = [],
    voiceInputUsed = false,
  ): Promise<void> {
    return this.json(`/chats/${chatId}/messages`, {
      method: "POST",
      headers: this.headers(true),
      body: JSON.stringify({
        turn_id: turnId,
        content,
        attachments,
        file_attachments: fileAttachments,
        invoked_skills: invokedSkills,
        voice_input_used: voiceInputUsed,
      }),
    });
  }

  steer(
    chatId: string,
    turnId: string,
    steerId: string,
    content: string,
    interrupt = false,
    voiceInputUsed = false,
    invokedSkills: readonly string[] = [],
  ): Promise<void> {
    return this.json(`/chats/${chatId}/steer`, {
      method: "POST",
      headers: this.headers(true),
      body: JSON.stringify({
        steer_id: steerId,
        turn_id: turnId,
        content,
        interrupt,
        voice_input_used: voiceInputUsed,
        invoked_skills: invokedSkills,
      }),
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
