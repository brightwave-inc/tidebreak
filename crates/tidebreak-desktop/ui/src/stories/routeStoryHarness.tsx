import type { ReactNode } from "react";

import {
  ApiClient,
  type AppDetail,
  type AppGrantState,
  type AppSummary,
  Chat,
  type ConnectedAppsInfo,
  ExecConfigInfo,
  InboxEntry,
  ManagedPolicy,
  type McpServerInfo,
  type ModelRoleInfo,
  PluginCatalog,
  Project,
  ProjectDocument,
  type VoiceTranscriptionInfo,
} from "@/api";
import { AppContextProvider, type AppContextValue } from "@/AppContext";
import { useChatAttention } from "@/ChatAttention";
import { useChatListStore } from "@/ChatListStore";
import { HOME_DRAFT_KEY, useComposerDrafts } from "@/ComposerDrafts";
import { useFirstMessage } from "@/FirstMessage";
import {
  FIRST_TASK_WALKTHROUGH_KEY,
  useFirstTaskGuide,
} from "@/FirstTaskWalkthrough";
import { useInbox } from "@/Inbox";
import {
  setFinishedNotificationsEnabled,
  setNeedsYouNotificationsEnabled,
} from "@/NotificationPreferences";
import { useNotifications } from "@/NotificationStore";
import { ManagedPolicyContext } from "@/managedPolicy";
import { useNewChatSettings } from "@/NewChatSettings";
import { useProjectListStore } from "@/ProjectListStore";
import { useChatsSectionState } from "@/sidebar/ChatsSection";
import { useUiStore } from "@/UiStore";
import { useVoiceInputStore } from "@/VoiceInputStore";
import { harnessDoctor, mcpDirectoryServers } from "./fixtures";
import {
  storyModels,
  storyProviders,
  storySettings,
} from "./SettingsStoryHarness";

export const routeProjects = [
  {
    id: "project-1",
    title: "Desktop release",
    attachment_revision: 4,
    root_attachments: [],
    instructions: "",
    created_at: "2026-08-20T10:00:00.000Z",
  },
  {
    id: "project-2",
    title: "Research archive",
    attachment_revision: 2,
    root_attachments: [],
    instructions: "",
    created_at: "2026-08-18T14:30:00.000Z",
  },
  {
    id: "project-3",
    title: "Customer onboarding and launch readiness",
    attachment_revision: 1,
    root_attachments: [],
    instructions: "",
    created_at: "2026-08-16T09:15:00.000Z",
  },
] satisfies Project[];

/** A project brief as someone would write one: short, specific, and plain. */
export const routeProjectInstructions = [
  "This project ships the 1.0 desktop release.",
  "",
  "- Treat the release checklist in docs/releases.md as the source of truth.",
  "- Name the pull request and the commit for every change you mention.",
  "- Flag anything that would break a 0.x profile on upgrade.",
].join("\n");

/**
 * A moment of activity relative to now, so the rail's date groups always have
 * rows whatever day the story renders. Day 0 is a few minutes ago; day N is
 * midday N calendar days back.
 */
export function activityAt(daysAgo: number, minutesAgo = 5): string {
  if (daysAgo === 0) {
    return new Date(Date.now() - minutesAgo * 60_000).toISOString();
  }
  const day = new Date();
  day.setHours(12, 0, 0, 0);
  day.setDate(day.getDate() - daysAgo);
  return new Date(day.getTime() - minutesAgo * 60_000).toISOString();
}

/** One conversation as the list serves it, with quiet defaults. */
export function routeChat(
  chat: Pick<Chat, "id" | "title"> & Partial<Chat>,
): Chat {
  const lastActivityAt = chat.last_activity_at ?? activityAt(0, 20);
  return {
    project_id: null,
    model: null,
    reasoning_effort: null,
    permission_mode: "ask",
    network_policy: { mode: "package_managers" },
    attachment_revision: 0,
    memory_incognito: false,
    root_attachments: [],
    created_at: lastActivityAt,
    last_activity_at: lastActivityAt,
    pinned_at: null,
    archived_at: null,
    running: false,
    unread: false,
    turn_count: 3,
    ...chat,
  };
}

const baseChats: Chat[] = [
  routeChat({
    id: "chat-1",
    title: "Review the updater migration",
    last_activity_at: activityAt(0, 2),
    running: true,
  }),
  routeChat({
    id: "chat-2",
    title: "Map plugin permission states",
    permission_mode: "plan",
    network_policy: { mode: "off" },
    last_activity_at: activityAt(0, 90),
    unread: true,
  }),
  routeChat({
    id: "chat-3",
    project_id: "project-1",
    title: "Make the dense sidebar easier to scan",
    network_policy: {
      mode: "allowed_hosts",
      allowed_hosts: ["github.com"],
      package_managers: false,
    },
    last_activity_at: activityAt(1),
  }),
];

export const routeChats: Chat[] = baseChats;

export const denseRouteChats: Chat[] = [
  ...baseChats,
  ...Array.from(
    { length: 12 },
    (_, index): Chat => ({
      ...baseChats[index % baseChats.length],
      id: `dense-chat-${index + 1}`,
      project_id: index % 4 === 0 ? "project-2" : null,
      title: [
        "Trace the shell loading state",
        "Prepare the release brief",
        "Check the connected app contract",
        "Audit navigation at narrow widths",
        "Summarize the permission review",
        "Organize the research handoff",
      ][index % 6],
      running: false,
      unread: false,
      last_activity_at: activityAt(index + 1),
    }),
  ),
];

/**
 * A list with every group in it: pinned work, today's running and unread
 * turns, and a tail that reaches back weeks. One untitled conversation that
 * nothing happened in is here too, and the rail leaves it out.
 */
export const groupedRouteChats: Chat[] = [
  routeChat({
    id: "pinned-1",
    title: "Quarterly board deck",
    pinned_at: activityAt(2),
    last_activity_at: activityAt(4),
  }),
  routeChat({
    id: "pinned-2",
    title: "Customer interview synthesis",
    pinned_at: activityAt(9),
    last_activity_at: activityAt(12),
  }),
  routeChat({
    id: "today-1",
    title: "Draft the launch announcement",
    last_activity_at: activityAt(0, 1),
    running: true,
  }),
  routeChat({
    id: "today-2",
    title: "Summarize the pricing survey",
    last_activity_at: activityAt(0, 35),
    unread: true,
  }),
  routeChat({
    id: "today-3",
    title: "Plan the offsite agenda",
    last_activity_at: activityAt(0, 70),
  }),
  routeChat({
    id: "today-4",
    title: "Review the updater migration",
    last_activity_at: activityAt(0, 120),
  }),
  routeChat({
    id: "yesterday-1",
    title: "Reconcile the September invoices",
    last_activity_at: activityAt(1, 60),
    unread: true,
  }),
  routeChat({
    id: "yesterday-2",
    title: "Compare vendor security reviews",
    last_activity_at: activityAt(1, 240),
  }),
  routeChat({
    id: "week-1",
    title: "Research competitor onboarding flows",
    last_activity_at: activityAt(3),
  }),
  routeChat({
    id: "week-2",
    title: "Outline the hiring plan for Q4",
    last_activity_at: activityAt(5),
  }),
  routeChat({
    id: "older-1",
    title: "Clean up the analytics dashboard",
    last_activity_at: activityAt(11),
  }),
  routeChat({
    id: "older-2",
    title: "Audit navigation at narrow widths",
    last_activity_at: activityAt(26),
  }),
  routeChat({
    id: "older-3",
    title: "Prepare the release brief",
    last_activity_at: activityAt(48),
  }),
  routeChat({
    id: "empty-1",
    title: null,
    turn_count: 0,
    last_activity_at: activityAt(0, 10),
  }),
];

/** Titles long enough to truncate at every rail width. */
export const longTitleRouteChats: Chat[] = [
  routeChat({
    id: "long-1",
    title:
      "Investigate why the nightly export to the finance data warehouse keeps timing out on the largest accounts",
    last_activity_at: activityAt(0, 3),
    running: true,
  }),
  routeChat({
    id: "long-2",
    title:
      "Rewrite the onboarding checklist so a new teammate can ship on day one without asking anyone",
    last_activity_at: activityAt(0, 50),
    unread: true,
  }),
  routeChat({
    id: "long-3",
    title:
      "Compare the three contract proposals line by line and flag every clause that shifts liability to us",
    pinned_at: activityAt(1),
    last_activity_at: activityAt(2),
  }),
  routeChat({
    id: "long-4",
    title: "Summarizethequarterlymetricsreportwithoutanyspacesatallinthetitle",
    last_activity_at: activityAt(6),
  }),
];

/** What the archive lists: out of the list, most recently archived first. */
export const archivedRouteChats: Chat[] = [
  routeChat({
    id: "archived-1",
    title: "Close out the Q2 vendor audit",
    archived_at: activityAt(0, 30),
    last_activity_at: activityAt(9),
  }),
  routeChat({
    id: "archived-2",
    project_id: "project-2",
    title: "Research notes for the pricing page",
    archived_at: activityAt(3),
    last_activity_at: activityAt(20),
  }),
  routeChat({
    id: "archived-3",
    title:
      "Draft the migration guide for customers moving off the legacy importer",
    archived_at: activityAt(14),
    last_activity_at: activityAt(40),
    running: true,
  }),
];

function waitingEntry(
  index: number,
  kind: InboxEntry["items"][number]["kind"],
  title: string,
): InboxEntry {
  const requestedAt = `2026-08-24T${String(12 - index).padStart(2, "0")}:00:00.000Z`;
  return {
    conversation: {
      sessionId: `waiting-chat-${index}`,
      workspaceId: null,
    },
    title,
    attention: {
      state: {
        type: "needs_you",
        prompt: "Waiting for your decision",
        source: "structured",
      },
      source: "structured",
    },
    items: [
      {
        turnId: `turn-${index}`,
        callId: `call-${index}`,
        kind,
        action: null,
        requestedAt,
      },
    ],
    waitingSince: requestedAt,
  };
}

export const denseInboxEntries: InboxEntry[] = [
  waitingEntry(1, "tool_approval", "Publish the release candidate"),
  waitingEntry(2, "question", "Choose the onboarding audience"),
  waitingEntry(3, "plan_review", "Review the settings migration plan"),
  waitingEntry(4, "folder_access", "Connect the customer research folder"),
  waitingEntry(5, "output_writeback", "Save the launch brief"),
  {
    conversation: {
      sessionId: "code-session-1",
      workspaceId: "workspace-1",
    },
    title: "Fix the desktop release workflow",
    attention: {
      state: {
        type: "needs_you",
        prompt: "Review the proposed changes",
        source: "structured",
      },
      source: "structured",
    },
    items: [],
    waitingSince: "2026-08-24T06:00:00.000Z",
  },
];

export const routeProjectDocuments = [
  {
    document_id: "document-1",
    media_type: "application/pdf",
    title: "Desktop release readiness.pdf",
    source_byte_len: 2_840_336,
    readable: true,
    created_at: "2026-08-24T12:00:00.000Z",
    updated_at: "2026-08-24T12:00:00.000Z",
  },
  {
    document_id: "document-2",
    media_type:
      "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    title: "Launch risks and owners.xlsx",
    source_byte_len: 468_412,
    readable: true,
    created_at: "2026-08-23T16:30:00.000Z",
    updated_at: "2026-08-23T16:30:00.000Z",
  },
  {
    document_id: "document-3",
    media_type:
      "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    title: "Executive launch review.pptx",
    source_byte_len: 8_451_920,
    readable: true,
    created_at: "2026-08-22T11:15:00.000Z",
    updated_at: "2026-08-22T11:15:00.000Z",
  },
  {
    document_id: "document-4",
    media_type: "text/markdown",
    title: "Release checklist and rollback notes.md",
    source_byte_len: 18_902,
    readable: true,
    created_at: "2026-08-21T09:45:00.000Z",
    updated_at: "2026-08-21T09:45:00.000Z",
  },
  {
    document_id: "document-5",
    media_type: "text/plain",
    title: null,
    source_byte_len: null,
    readable: false,
    created_at: "2026-08-20T08:10:00.000Z",
    updated_at: "2026-08-20T08:10:00.000Z",
  },
] satisfies ProjectDocument[];

export const unmanagedPolicy: ManagedPolicy = {
  managed: false,
  source: "unmanaged",
  misconfigured: false,
  allow_local_mcp_servers: false,
};

export const managedPolicy: ManagedPolicy = {
  managed: true,
  source: "provisioned",
  gateway_url: "https://gateway.example.com",
  misconfigured: false,
  allow_local_mcp_servers: false,
};

export const routePluginCatalog: PluginCatalog = {
  plugins: [
    {
      name: "document-work",
      display_name: "Document work",
      description: "Create, inspect, and revise office documents and PDFs.",
      category: "documents",
      origin: "builtin",
      capabilities: ["write-files"],
      compatibility: { status: "compatible", issues: [] },
      enabled: true,
      skills: [
        {
          name: "documents",
          description: "Create, edit, and review Word documents.",
          origin: "builtin",
          enabled: true,
        },
        {
          name: "pdf",
          description: "Read, create, render, and inspect PDF files.",
          origin: "builtin",
          enabled: true,
        },
      ],
    },
    {
      name: "data-workbench",
      display_name: "Data workbench",
      description: "Analyze spreadsheets and produce verified data outputs.",
      category: "data",
      origin: "builtin",
      capabilities: ["write-files", "host-install"],
      compatibility: {
        status: "limited",
        issues: [
          {
            kind: "missing_sandbox_dependency",
            skill: "spreadsheets",
            dependency: "libreoffice",
          },
        ],
      },
      enabled: false,
      skills: [
        {
          name: "spreadsheets",
          description: "Create, edit, analyze, and verify spreadsheet files.",
          origin: "builtin",
          enabled: true,
        },
      ],
    },
    {
      name: "browser-research",
      display_name: "Browser research",
      description: "Inspect websites and collect evidence from live sources.",
      category: "other",
      origin: "user",
      capabilities: ["network", "live-control", "mcp"],
      compatibility: { status: "unchecked", issues: [] },
      enabled: true,
      skills: [],
    },
  ],
  skills: [
    {
      name: "release-notes",
      description: "Turn merged changes into release notes.",
      origin: "user",
      enabled: true,
    },
  ],
  prompts: [],
};

export const routeApps = [
  {
    id: "release-brief",
    name: "Release brief",
    revision_count: 4,
    updated_at: "2026-08-23T16:40:00.000Z",
    granted: true,
  },
  {
    id: "incident-map",
    name: "Incident map",
    revision_count: 1,
    updated_at: "2026-08-22T09:15:00.000Z",
    granted: false,
  },
  {
    id: "research-table",
    name: "Research source table with a deliberately long title",
    revision_count: 7,
    updated_at: "2026-08-19T18:20:00.000Z",
    granted: true,
  },
] satisfies AppSummary[];

export const routeAppDetail = {
  id: "release-brief",
  name: "Release brief",
  created_at: "2026-08-18T10:10:00.000Z",
  updated_at: "2026-08-23T16:40:00.000Z",
  current_revision: "revision-4",
  revisions: [
    {
      id: "revision-4",
      ordinal: 4,
      created_at: "2026-08-23T16:40:00.000Z",
    },
    {
      id: "revision-3",
      ordinal: 3,
      created_at: "2026-08-22T15:20:00.000Z",
    },
    {
      id: "revision-2",
      ordinal: 2,
      created_at: "2026-08-20T13:00:00.000Z",
    },
  ],
} satisfies AppDetail;

const routeAppGrant = {
  granted: true,
  bindings: [
    {
      app: null,
      folder: "folder-release-notes",
      gateway_app: null,
      access: "read_write",
      name: "Release notes",
      operation_ids: null,
      granted: true,
      definition_changed: false,
    },
    {
      app: null,
      folder: null,
      gateway_app: "github",
      access: null,
      name: "GitHub",
      operation_ids: ["pulls.list", "pulls.comment", "checks.read"],
      granted: true,
      definition_changed: false,
    },
  ],
} satisfies AppGrantState;

const routeModelRoles = [
  {
    role: "chat",
    selection: storyModels[0].key,
    resolved_key: storyModels[0].key,
  },
  {
    role: "utility",
    selection: null,
    resolved_key: storyModels[1].key,
  },
] satisfies ModelRoleInfo[];

const routeVoiceInfo = {
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
  ],
  openai_ready: true,
  gemini_ready: true,
} satisfies VoiceTranscriptionInfo;

const executionConfig = {
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
    {
      provider: "daytona",
      admitted: false,
      denials: ["image_not_verified"],
    },
    { provider: "docker", admitted: false, denials: ["image_not_verified"] },
  ],
} satisfies ExecConfigInfo;

const routeMcpServer = {
  name: "docs",
  command: "/usr/local/bin/docs-mcp",
  args: [],
  env: [],
  env_from: [],
  cwd: null,
  url: null,
  bearer_token_env: null,
  oauth: false,
  gateway_endpoint: null,
  request_timeout_ms: 60_000,
  enabled: true,
  plugin: null,
  health: "healthy",
  tool_count: 3,
  diagnostic: null,
  curated: null,
} satisfies McpServerInfo;

const routeConnectedApps = {
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
      allow_loopback_http: false,
    },
  ],
  skipped_mcp_servers: [],
} satisfies ConnectedAppsInfo;

type RouteClientMethods = Pick<
  ApiClient,
  | "getSettings"
  | "putSettings"
  | "listProviders"
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
  | "getWebSearchConfig"
  | "putWebSearchConfig"
  | "listWebSearchCredentials"
  | "putWebSearchCredential"
  | "deleteWebSearchCredential"
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
  | "discoverRestSpec"
  | "deleteRestConnectedApp"
  | "listMcpServers"
  | "putMcpServers"
  | "reconnectMcpServer"
  | "getMcpDirectory"
  | "addMcpDirectoryServer"
  | "removeSkippedMcpServer"
  | "getGatewayStatus"
  | "gatewaySignIn"
  | "gatewaySignOut"
  | "getGatewayMachine"
  | "syncGatewayModels"
  | "getGatewayApps"
  | "listConsentStatements"
  | "revokeStandingGrant"
  | "getPersonalInstructions"
  | "putPersonalInstructions"
  | "patchProjectInstructions"
  | "listProjectDocuments"
  | "listApps"
  | "getApp"
  | "deleteApp"
  | "getAppGrant"
  | "consentAppGrant"
  | "revokeAppGrant"
  | "createAppViewFrame"
  | "invokeAppOperation"
  | "invokeAppGatewayOperation"
  | "invokeAppFolder"
  | "appGatewayPage"
  | "listPlugins"
  | "setPluginsEnabled"
  | "getSkillInstructions"
  | "getPromptBody"
  | "listNotifications"
  | "notificationUnreadCount"
  | "markNotificationsRead"
  | "markAllNotificationsRead"
  | "listChats"
>;

export function pending<T>(): Promise<T> {
  return new Promise(() => undefined);
}

export function storyClient(
  overrides: Partial<RouteClientMethods> = {},
): ApiClient {
  const methods: RouteClientMethods = {
    getSettings: async () => storySettings,
    putSettings: async () => storySettings,
    listProviders: async () => ({ providers: storyProviders }),
    putProvider: async (kind) =>
      storyProviders.find((provider) => provider.kind === kind) ??
      storyProviders[0],
    deleteCredential: async () => {},
    getOpenaiChatgptStatus: async () => ({
      signed_in: true,
      account_hint: "alex@example.com",
    }),
    openaiChatgptSignIn: async () => ({
      authorization_url: "https://auth.example.test",
    }),
    openaiChatgptSignOut: async () => {},
    listModels: async () => ({
      models: storyModels,
      roles: routeModelRoles,
    }),
    putModelRole: async (role, selection) => ({
      role,
      selection,
      resolved_key:
        selection ??
        routeModelRoles.find((entry) => entry.role === role)?.resolved_key ??
        storyModels[0].key,
    }),
    getVoiceTranscription: async () => routeVoiceInfo,
    putVoiceTranscription: async (model, localModel) => ({
      ...routeVoiceInfo,
      model,
      local_model: localModel ?? routeVoiceInfo.local_model,
    }),
    installLocalVoice: async () => ({
      state: "ready",
      downloaded_bytes: 148_000_000,
      total_bytes: 148_000_000,
      error: null,
    }),
    getWebSearchConfig: async () => ({
      provider: "exa",
      mode: "host",
      timeout_ms: 20_000,
      has_credential: true,
      available: true,
    }),
    putWebSearchConfig: async () => ({
      provider: "exa",
      mode: "host",
      timeout_ms: 20_000,
      has_credential: true,
      available: true,
    }),
    listWebSearchCredentials: async () => ({
      credentials: [
        { provider: "exa", has_credential: true },
        { provider: "tavily", has_credential: false },
        { provider: "brave", has_credential: false },
        { provider: "searxng", has_credential: false },
        { provider: "model_provider", has_credential: false },
      ],
    }),
    putWebSearchCredential: async (provider) => ({
      provider,
      has_credential: true,
    }),
    deleteWebSearchCredential: async (provider) => ({
      provider,
      has_credential: false,
    }),
    getExecConfig: async () => executionConfig,
    putExecConfig: async () => executionConfig,
    listExecCredentials: async () => ({
      credentials: [
        { provider: "e2b", has_credential: true },
        { provider: "daytona", has_credential: true },
      ],
    }),
    putExecCredential: async (provider) => ({
      provider,
      has_credential: true,
    }),
    deleteExecCredential: async (provider) => ({
      provider,
      has_credential: false,
    }),
    getHarnessDoctor: async () => harnessDoctor,
    refreshHarnessDoctor: async () => harnessDoctor,
    getCodeWorktreeRoot: async () => ({
      root: "/Users/alex/Code/worktrees",
      effective_root: "/Users/alex/Code/worktrees",
      default_root: "/Users/alex/.tidebreak/worktrees",
    }),
    setCodeWorktreeRoot: async (root) => ({
      ...(root ? { root } : {}),
      effective_root: root ?? "/Users/alex/.tidebreak/worktrees",
      default_root: "/Users/alex/.tidebreak/worktrees",
    }),
    listConnectedApps: async () => routeConnectedApps,
    putRestConnectedApp: async () => routeConnectedApps,
    previewRestSpec: async () => ({
      document_sha256: "cd".repeat(32),
      operations: [],
      unlistable: 0,
      truncated: false,
    }),
    discoverRestSpec: async () => ({
      candidates: [],
      tried: [
        "https://api.example.com/openapi.json",
        "https://api.example.com/swagger.json",
      ],
    }),
    deleteRestConnectedApp: async () => {},
    listMcpServers: async () => ({ servers: [routeMcpServer] }),
    putMcpServers: async () => ({ servers: [routeMcpServer] }),
    reconnectMcpServer: async () => ({ servers: [routeMcpServer] }),
    getMcpDirectory: async () => ({ servers: mcpDirectoryServers }),
    addMcpDirectoryServer: async (id) => ({
      name: id,
      servers: [routeMcpServer],
    }),
    removeSkippedMcpServer: async () => {},
    getGatewayStatus: async () => ({
      base_url: "https://gateway.example.test",
      signed_in: true,
      account_hint: "alex@example.com",
      installation_id: "installation-story",
      model_count: 2,
      member_catalog: "2026-08-01",
      sign_in: { state: "idle" },
    }),
    gatewaySignIn: async () => ({
      authorization_url: "https://gateway.example.test/sign-in",
    }),
    gatewaySignOut: async () => ({
      base_url: "https://gateway.example.test",
      signed_in: false,
      model_count: 0,
      sign_in: { state: "idle" },
    }),
    getGatewayMachine: async () => ({
      url: "https://machine.gateway.example.test",
    }),
    syncGatewayModels: async () => ({
      base_url: "https://gateway.example.test",
      signed_in: true,
      account_hint: "alex@example.com",
      installation_id: "installation-story",
      model_count: 2,
      member_catalog: "2026-08-01",
      sign_in: { state: "idle" },
    }),
    getGatewayApps: async () => ({
      supported: true,
      apps: [
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
    listConsentStatements: async () => [],
    revokeStandingGrant: async () => {},
    getPersonalInstructions: async () => ({ instructions: "" }),
    putPersonalInstructions: async (instructions) => ({ instructions }),
    patchProjectInstructions: async (projectId, instructions) => ({
      ...(routeProjects.find((project) => project.id === projectId) ??
        routeProjects[0]),
      instructions,
    }),
    listProjectDocuments: async () => ({
      documents: routeProjectDocuments,
      next_cursor: null,
    }),
    listApps: async () => ({ apps: routeApps }),
    getApp: async () => routeAppDetail,
    deleteApp: async () => {},
    getAppGrant: async () => routeAppGrant,
    consentAppGrant: async () => routeAppGrant,
    revokeAppGrant: async () => {},
    createAppViewFrame: async () => ({
      frame_path: `data:text/html;charset=utf-8,${encodeURIComponent(
        "<!doctype html><html><body><main><h1>Desktop release readiness</h1><p>17 checks passed. Two reviews remain open.</p></main></body></html>",
      )}`,
    }),
    invokeAppOperation: async () => ({ is_error: false }),
    invokeAppGatewayOperation: async () => ({ is_error: false }),
    invokeAppFolder: async () => ({ is_error: false }),
    appGatewayPage: async () => ({
      outcome: "ready",
      url: "https://gateway.example.test/apps/release-brief",
    }),
    listPlugins: async () => routePluginCatalog,
    setPluginsEnabled: async () => routePluginCatalog,
    getSkillInstructions: async (name) => ({
      name,
      instructions:
        "# Review the source\n\nRead the source before you edit it. Preserve facts and links.",
    }),
    getPromptBody: async (name) => ({
      name,
      body: "Summarize the launch risks and assign a clear owner to each one.",
    }),
    listNotifications: async () => ({ notifications: [], nextCursor: null }),
    notificationUnreadCount: async () => 0,
    markNotificationsRead: async () => 0,
    markAllNotificationsRead: async () => 0,
    listChats: async (options) =>
      options?.archived ? archivedRouteChats : routeChats,
    ...overrides,
  };

  return Object.assign(
    new ApiClient("https://route-story.invalid", "storybook"),
    methods,
  );
}

const idleUpdateState: AppContextValue["updateState"] = {
  status: "idle",
  version: null,
  error: null,
  enabled: true,
};

export function storyAppContext(
  client: ApiClient,
  overrides: Partial<AppContextValue> = {},
): AppContextValue {
  return {
    client,
    attachment: "local",
    models: storyModels,
    defaultModelKey: null,
    providers: storyProviders,
    refreshCatalog: async () => {},
    refreshChats: async () => {
      useChatListStore.setState({ chatsError: null });
    },
    status: "",
    setStatus: () => {},
    newChat: () => {},
    deleteChat: () => {},
    togglePinChat: () => {},
    archiveChat: () => {},
    unarchiveChat: () => {},
    startRename: () => {},
    commitRename: () => {},
    cancelRename: () => {},
    newProject: async () => false,
    deleteProject: () => {},
    startProjectRename: () => {},
    commitProjectRename: () => {},
    cancelProjectRename: () => {},
    newChatInProject: () => {},
    moveChatToProject: () => {},
    updateState: idleUpdateState,
    updateUpToDate: false,
    checkForUpdate: async () => idleUpdateState,
    restartForUpdate: async () => {},
    ...overrides,
  };
}

type StoreFixture = {
  chats?: Chat[];
  archivedChats?: Chat[];
  archivedLoaded?: boolean;
  chatsLoaded?: boolean;
  chatsError?: string | null;
  creatingChat?: boolean;
  projects?: Project[];
  projectsLoaded?: boolean;
  expandedProjectIds?: string[];
  inboxEntries?: InboxEntry[];
  inboxLoaded?: boolean;
  attentionChatIds?: string[];
  sidebarWidth?: number;
  sidebarCollapsed?: boolean;
};

export function resetRouteStoryStores({
  chats = routeChats,
  archivedChats = [],
  archivedLoaded = false,
  chatsLoaded = true,
  chatsError = null,
  creatingChat = false,
  projects = routeProjects,
  projectsLoaded = true,
  expandedProjectIds = ["project-1"],
  inboxEntries = [],
  inboxLoaded = true,
  attentionChatIds = [],
  sidebarWidth = 280,
  sidebarCollapsed = false,
}: StoreFixture = {}): void {
  useChatListStore.setState({
    chats,
    archivedChats,
    archivedLoaded,
    chatsLoaded,
    chatsError,
    creatingChat,
    deletingChatId: null,
    renamingChatId: null,
    renameChatDraft: "",
    savingTitle: false,
    derivedTitleChatId: null,
    streamedTitles: {},
  });
  useProjectListStore.setState({
    projects,
    projectsLoaded,
    creatingProject: false,
    deletingProjectId: null,
    renamingProjectId: null,
    renameProjectDraft: "",
    savingProjectTitle: false,
    expandedProjectIds,
  });
  useInbox.setState({ entries: inboxEntries, loaded: inboxLoaded });
  useNotifications.setState({
    notifications: [],
    unread: 0,
    loaded: true,
  });
  useChatAttention.setState({
    chatIdsWithPendingPrompts: new Set(attentionChatIds),
  });
  useChatsSectionState.setState({
    collapsed: false,
    filtering: false,
    query: "",
    showAllOlder: false,
  });
  useUiStore.setState({
    sidebarCollapsed,
    sidebarWidth,
    modelMenuNotConnectedCollapsed: false,
    activeTurnSendMode: "queue",
  });
  useComposerDrafts.getState().clearDraft(HOME_DRAFT_KEY);
  useComposerDrafts.setState({ drafts: {}, attachments: {} });
  useNewChatSettings.setState({
    defaults: null,
    model: null,
    reasoningEffort: null,
    permissionMode: null,
    networkPolicy: null,
  });
  useFirstMessage.setState({ chatId: null, pending: null });
  useFirstTaskGuide.setState({ surface: null });
  useVoiceInputStore.setState({
    info: null,
    loading: false,
    installing: null,
    error: null,
  });
  try {
    window.localStorage.setItem(FIRST_TASK_WALKTHROUGH_KEY, "skipped");
  } catch {
    // Story fixtures remain deterministic even when storage is unavailable.
  }
  // A notification story can turn these off, and storage outlives it.
  setNeedsYouNotificationsEnabled(true);
  setFinishedNotificationsEnabled(true);
}

export function RouteStoryProviders({
  client,
  context,
  policy = unmanagedPolicy,
  children,
}: {
  client: ApiClient;
  context?: Partial<AppContextValue>;
  policy?: ManagedPolicy;
  children: ReactNode;
}) {
  return (
    <ManagedPolicyContext.Provider value={policy}>
      <AppContextProvider value={storyAppContext(client, context)}>
        {children}
      </AppContextProvider>
    </ManagedPolicyContext.Provider>
  );
}
