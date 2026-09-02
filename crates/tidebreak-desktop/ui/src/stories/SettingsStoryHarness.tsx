import {
  createMemoryHistory,
  createRootRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import type { ReactNode } from "react";
import { useLayoutEffect, useMemo } from "react";

import {
  ApiClient,
  type ConnectedAppsInfo,
  type ConsentStatementSnapshot,
  type ExecConfigInfo,
  type ExecCredentialReadiness,
  type McpServerInfo,
  type ModelInfo,
  type ModelRoleInfo,
  type ProviderInfo,
  type RuntimeSettings,
  type VoiceTranscriptionInfo,
} from "@/api";
import { useVoiceInputStore } from "@/VoiceInputStore";
import { harnessDoctor } from "./fixtures";

export type SettingsStoryState =
  | "configured"
  | "loading"
  | "managed"
  | "disabled"
  | "empty"
  | "failed";

type SettingsClientMethods = Pick<
  ApiClient,
  | "getSettings"
  | "putSettings"
  | "putProvider"
  | "deleteCredential"
  | "getOpenaiChatgptStatus"
  | "openaiChatgptSignIn"
  | "openaiChatgptSignOut"
  | "listModels"
  | "putModelRole"
  | "getVoiceTranscription"
  | "putVoiceTranscription"
  | "installLocalVoice"
  | "getExecConfig"
  | "putExecConfig"
  | "listExecCredentials"
  | "putExecCredential"
  | "deleteExecCredential"
  | "getHarnessDoctor"
  | "refreshHarnessDoctor"
  | "getCodeWorktreeRoot"
  | "setCodeWorktreeRoot"
  | "listConnectedApps"
  | "putRestConnectedApp"
  | "previewRestSpec"
  | "deleteRestConnectedApp"
  | "listMcpServers"
  | "putMcpServers"
  | "reconnectMcpServer"
  | "getGatewayStatus"
  | "getGatewayApps"
  | "listConsentStatements"
  | "revokeStandingGrant"
>;

export const storySettings: RuntimeSettings = {
  model: null,
  has_api_key: false,
  chat_defaults: {
    model: null,
    reasoning_effort: null,
    permission_mode: null,
    network_policy: null,
  },
  max_active_background_agents: 6,
  sandbox_agent_checkin_steps: 18,
  sandbox_agent_error_checkin: 4,
  compaction: {
    threshold_fraction: 0.78,
    target_fraction: 0.52,
    min_threshold_tokens: 16_000,
    protect_recent_messages: 8,
  },
  model_visibility_overrides: {},
  prompt_cache_retention: "five_minutes",
  computer_use_enabled: true,
  code_turn_recaps_enabled: true,
  rewrite_closing_messages: false,
  harness_update_channel: "pinned",
  git_source_control: {
    auto_rename_branches: true,
    branch_prefix_mode: "account",
    account_prefix: "alex/",
    effective_branch_prefix: "alex/",
  },
  memory: { enabled: true, capture_enabled: false, capture_ready: false },
};

function model(
  provider: ModelInfo["provider"],
  id: string,
  displayName: string,
  available = true,
): ModelInfo {
  return {
    key: `${provider}::${id}`,
    id,
    display_name: displayName,
    provider,
    vendor: null,
    verification: "verified",
    available,
    context_window: 200_000,
    max_output_tokens: 32_000,
    input_modalities: ["text", "image"],
    supports_reasoning: true,
    supports_tools: true,
    supports_structured_output: true,
    reasoning_efforts: ["low", "medium", "high"],
    multimodal: true,
    recommended: true,
  };
}

const openModels: ModelInfo[] = [
  model("anthropic", "claude-sonnet-4-6", "Claude Sonnet 4.6"),
  model("openai", "gpt-5.4", "GPT-5.4"),
  model("gemini", "gemini-3.6-pro", "Gemini 3.6 Pro"),
  model("openrouter", "moonshotai/kimi-k3", "Kimi K3"),
];

const managedModels: ModelInfo[] = [
  model("model_gateway", "flagship", "Gateway Flagship"),
  model("model_gateway", "fast", "Gateway Fast"),
];

const openRoles: ModelRoleInfo[] = [
  {
    role: "chat",
    selection: "anthropic::claude-sonnet-4-6",
    resolved_key: "anthropic::claude-sonnet-4-6",
  },
  {
    role: "utility",
    selection: null,
    resolved_key: "openai::gpt-5.4",
  },
];

const managedRoles: ModelRoleInfo[] = [
  {
    role: "chat",
    selection: null,
    resolved_key: "model_gateway::flagship",
  },
  {
    role: "utility",
    selection: null,
    resolved_key: "model_gateway::fast",
  },
];

export const storyProviders: ProviderInfo[] = [
  {
    kind: "anthropic",
    enabled: true,
    has_credential: true,
    models: [],
  },
  {
    kind: "openai",
    enabled: true,
    has_credential: true,
    auth_mode: "chatgpt",
    models: [],
  },
  {
    kind: "gemini",
    enabled: false,
    has_credential: false,
    models: [],
  },
  {
    kind: "ollama",
    enabled: true,
    has_credential: false,
    base_url: "http://127.0.0.1:11434/v1",
    models: [
      {
        id: "qwen3:8b",
        display_name: "Qwen 3 8B",
        context_window: 32_768,
        max_output_tokens: 8_192,
        input_modalities: ["text"],
        supports_reasoning: true,
        reasoning_efforts: ["low", "medium", "high"],
      },
    ],
  },
  {
    kind: "openrouter",
    enabled: true,
    has_credential: true,
    base_url: "https://openrouter.ai/api/v1",
    models: [],
  },
];

export const storyModels = openModels;

const readyVoice: VoiceTranscriptionInfo = {
  model: "local",
  local_model: "whisper-base-en",
  local_models: [
    {
      id: "whisper-base-en",
      label: "Whisper Base English",
      description: "A balanced local model for everyday dictation.",
      total_bytes: 148_000_000,
      english_only: true,
      recommended: true,
      state: "ready",
      downloaded_bytes: 148_000_000,
      error: null,
    },
    {
      id: "whisper-large-v3",
      label: "Whisper Large v3",
      description: "Higher accuracy across languages with a larger download.",
      total_bytes: 1_550_000_000,
      english_only: false,
      recommended: false,
      state: "not_installed",
      downloaded_bytes: null,
      error: null,
    },
  ],
  openai_ready: true,
  gemini_ready: true,
};

const disabledVoice: VoiceTranscriptionInfo = {
  ...readyVoice,
  local_models: readyVoice.local_models.map((entry) => ({
    ...entry,
    state: "unavailable" as const,
    downloaded_bytes: null,
  })),
  openai_ready: false,
  gemini_ready: false,
};

const configuredExec: ExecConfigInfo = {
  provider: "daytona",
  timeout_ms: 30_000,
  available: true,
  has_credential: true,
  providers: [
    { provider: "local", available: true },
    { provider: "e2b", available: true },
    { provider: "daytona", available: true },
    { provider: "docker", available: true },
  ],
  egress: {
    policy: { mode: "open" },
    enforcement: [
      {
        provider: "e2b",
        status: "applied_with_gaps",
        gaps: ["DNS resolution"],
      },
      {
        provider: "daytona",
        status: "conditional_boundary",
        gaps: [],
        requirement: "Daytona org tier 3+",
      },
    ],
  },
  detached_admission: [
    { provider: "local", admitted: false, denials: ["image_not_verified"] },
    { provider: "e2b", admitted: false, denials: ["image_not_verified"] },
    { provider: "daytona", admitted: false, denials: ["image_not_verified"] },
    { provider: "docker", admitted: false, denials: ["image_not_verified"] },
  ],
};

const disabledExec: ExecConfigInfo = {
  ...configuredExec,
  provider: undefined,
  available: false,
  has_credential: false,
  providers: [
    {
      provider: "local",
      available: false,
      unavailable_reason: "unsupported_platform",
    },
    {
      provider: "e2b",
      available: false,
      unavailable_reason: "missing_credential",
    },
    {
      provider: "daytona",
      available: false,
      unavailable_reason: "missing_credential",
    },
    {
      provider: "docker",
      available: false,
      unavailable_reason: "missing_container_runtime",
    },
  ],
};

const execCredentials: ExecCredentialReadiness[] = [
  { provider: "e2b", has_credential: true },
  { provider: "daytona", has_credential: true },
];

const docsServer: McpServerInfo = {
  name: "docs",
  command: "/usr/local/bin/docs-mcp",
  args: [],
  env: [],
  env_from: [],
  cwd: null,
  url: null,
  bearer_token_env: null,
  gateway_endpoint: null,
  request_timeout_ms: 60_000,
  enabled: true,
  plugin: null,
  health: "healthy",
  tool_count: 3,
  diagnostic: null,
  curated: null,
};

const gatewayServer: McpServerInfo = {
  ...docsServer,
  name: "primary",
  command: null,
  gateway_endpoint: "primary",
  tool_count: 4,
};

const connectedApps: ConnectedAppsInfo = {
  apps: [
    {
      kind: "mcp_server",
      id: "app-docs",
      name: "docs",
      health: "healthy",
      tool_count: 3,
      tools: ["search", "fetch", "list_documents"],
      diagnostic: null,
      curated: null,
      gateway_endpoint: null,
      gateway_apps: [],
      used_by_app_count: 2,
    },
    {
      kind: "mcp_server",
      id: "app-primary",
      name: "primary",
      health: "healthy",
      tool_count: 4,
      tools: ["search_issues", "create_issue", "get_customer", "post_note"],
      diagnostic: null,
      curated: null,
      gateway_endpoint: "primary",
      gateway_apps: ["Linear", "Salesforce"],
      used_by_app_count: 3,
    },
    {
      kind: "rest_api",
      id: "app-sentry",
      name: "Sentry",
      base_url: "https://api.sentry.example/v2",
      operation_count: 12,
      document_sha256: "ab".repeat(32),
      credential_status: "configured",
      placement: "bearer",
      updated_at: "2026-08-20T12:00:00Z",
      used_by_app_count: 1,
    },
  ],
};

const consentStatements: ConsentStatementSnapshot[] = [
  {
    handle: { kind: "tool_grant", call_id: "grant-cargo" },
    level: { level: "chat", chat_id: "chat-filings" },
    level_title: "Quarterly filings",
    verb: {
      kind: "tool",
      action: "exec",
      approval: "exec_may_run_networked_command",
    },
    resource: {
      kind: "action_scope",
      scope: { scope: "any_args_for", command: "cargo" },
    },
    method: "approval_card",
    granted_at: "2026-08-18T12:00:00Z",
  },
  {
    handle: { kind: "tool_grant", call_id: "grant-web" },
    level: { level: "project", project_id: "project-launch" },
    level_title: "Launch readiness",
    verb: {
      kind: "tool",
      action: "web_extract",
      approval: "web_extract_may_fetch_url",
    },
    resource: {
      kind: "action_scope",
      scope: { scope: "whole_tool" },
    },
    method: "carried_forward",
    granted_at: "2026-08-19T16:30:00Z",
  },
];

function pending<T>(): Promise<T> {
  return new Promise(() => undefined);
}

function createSettingsStoryClient(
  state: SettingsStoryState,
  initialSettings: RuntimeSettings,
): ApiClient {
  let settings = initialSettings;
  const read = <T,>(value: T): Promise<T> => {
    if (state === "loading") return pending();
    if (state === "failed") {
      return Promise.reject(new Error("Settings could not be loaded."));
    }
    return Promise.resolve(value);
  };
  const write = <T,>(value: T): Promise<T> => {
    if (state === "failed") {
      return Promise.reject(new Error("Settings could not be saved."));
    }
    return Promise.resolve(value);
  };
  const models = state === "managed" ? managedModels : openModels;
  const roles = state === "managed" ? managedRoles : openRoles;
  const voice = state === "disabled" ? disabledVoice : readyVoice;
  const exec = state === "disabled" ? disabledExec : configuredExec;
  const servers = state === "managed" ? [gatewayServer] : [docsServer];

  const methods: SettingsClientMethods = {
    getSettings: () => read(settings),
    putSettings: (body) => {
      settings = {
        ...settings,
        ...body,
        compaction: body.compaction
          ? { ...settings.compaction, ...body.compaction }
          : settings.compaction,
        git_source_control: body.git_source_control
          ? {
              ...settings.git_source_control,
              ...body.git_source_control,
              custom_branch_prefix:
                body.git_source_control.custom_branch_prefix === null
                  ? undefined
                  : (body.git_source_control.custom_branch_prefix ??
                    settings.git_source_control.custom_branch_prefix),
            }
          : settings.git_source_control,
        memory: body.memory
          ? {
              ...settings.memory,
              ...body.memory,
              capture_ready:
                (body.memory.enabled ?? settings.memory.enabled) &&
                (body.memory.capture_enabled ??
                  settings.memory.capture_enabled),
            }
          : settings.memory,
      };
      return write(settings);
    },
    putProvider: (kind) =>
      write(storyProviders.find((provider) => provider.kind === kind)!),
    deleteCredential: () => write(undefined),
    getOpenaiChatgptStatus: () =>
      read({ signed_in: true, account_hint: "alex@example.com" }),
    openaiChatgptSignIn: () =>
      write({ authorization_url: "https://auth.example.test" }),
    openaiChatgptSignOut: () => write(undefined),
    listModels: () => read({ models, roles }),
    putModelRole: (role, selection) =>
      write({
        role,
        selection,
        resolved_key:
          selection ?? roles.find((entry) => entry.role === role)!.resolved_key,
      }),
    getVoiceTranscription: () => read(voice),
    putVoiceTranscription: (selection, localModel) =>
      write({
        ...voice,
        model: selection,
        local_model: localModel ?? voice.local_model,
      }),
    installLocalVoice: () =>
      write({
        state: "ready",
        downloaded_bytes: 148_000_000,
        total_bytes: 148_000_000,
        error: null,
      }),
    getExecConfig: () => read(exec),
    putExecConfig: () => write(exec),
    listExecCredentials: () => read({ credentials: execCredentials }),
    putExecCredential: (provider) => write({ provider, has_credential: true }),
    deleteExecCredential: (provider) =>
      write({ provider, has_credential: false }),
    getHarnessDoctor: () => read(harnessDoctor),
    refreshHarnessDoctor: () => read(harnessDoctor),
    getCodeWorktreeRoot: () =>
      read({
        root: "/Users/alex/Code/worktrees",
        effective_root: "/Users/alex/Code/worktrees",
        default_root: "/Users/alex/.tidebreak/worktrees",
      }),
    setCodeWorktreeRoot: (root) =>
      write({
        ...(root ? { root } : {}),
        effective_root: root ?? "/Users/alex/.tidebreak/worktrees",
        default_root: "/Users/alex/.tidebreak/worktrees",
      }),
    listConnectedApps: () =>
      read(state === "empty" ? { apps: [] } : connectedApps),
    putRestConnectedApp: () => write(connectedApps),
    previewRestSpec: () =>
      write({
        document_sha256: "cd".repeat(32),
        operations: [],
        unlistable: 0,
        truncated: false,
      }),
    deleteRestConnectedApp: () => write(undefined),
    listMcpServers: () => read({ servers: state === "empty" ? [] : servers }),
    putMcpServers: () => write({ servers }),
    reconnectMcpServer: () => write({ servers }),
    getGatewayStatus: () =>
      read({
        base_url: "https://gateway.example.test",
        signed_in: state === "managed",
        account_hint: "alex@example.com",
        installation_id: "installation-story",
        model_count: managedModels.length,
        member_catalog: "2026-08-01",
        sign_in: { state: "idle" },
      }),
    getGatewayApps: () =>
      read({
        supported: true,
        apps:
          state === "empty"
            ? []
            : [
                {
                  id: "gateway-linear",
                  name: "Linear",
                  app_kind: "mcp_server",
                  enabled: true,
                  mcp_endpoint_slugs: ["primary"],
                  connection: "ready",
                  used_by_app_count: 2,
                },
              ],
      }),
    listConsentStatements: () =>
      read(state === "empty" ? [] : consentStatements),
    revokeStandingGrant: () => write(undefined),
  };

  return Object.assign(
    new ApiClient("https://settings-story.invalid", "storybook"),
    methods,
  );
}

function SettingsStoryRouter({ children }: { children: ReactNode }) {
  const router = useMemo(() => {
    const rootRoute = createRootRoute({ component: () => children });
    return createRouter({
      routeTree: rootRoute,
      history: createMemoryHistory({ initialEntries: ["/settings"] }),
    });
  }, [children]);
  return <RouterProvider router={router as never} />;
}

export function SettingsStoryHarness({
  state = "configured",
  settings = storySettings,
  children,
}: {
  state?: SettingsStoryState;
  settings?: RuntimeSettings;
  children: (client: ApiClient) => ReactNode;
}) {
  const client = useMemo(
    () => createSettingsStoryClient(state, settings),
    [settings, state],
  );

  useLayoutEffect(() => {
    useVoiceInputStore.setState({
      info: null,
      loading: false,
      installing: null,
      error: null,
    });
  }, [client]);

  window.localStorage.removeItem("tidebreak.settings.providers-expanded");
  return <SettingsStoryRouter>{children(client)}</SettingsStoryRouter>;
}
