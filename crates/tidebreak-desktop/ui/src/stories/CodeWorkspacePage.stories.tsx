import { useEffect, useState } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import { expect, fireEvent, userEvent, within } from "storybook/test";

import { AppContextProvider, type AppContextValue } from "@/AppContext";
import type { ApiClient } from "@/api/client";
import type {
  CodeRepoSnapshot,
  CodeSessionDigest,
  CodeSessionSnapshot,
  CodeTurnSnapshot,
  CodeWorkspacePrSnapshot,
  CodeWorkspaceSnapshot,
  PullRequestDigest,
  SequencedCodeEventFrame,
} from "@/api/types";
import { useCodeCatalogStore } from "@/code/CodeCatalogStore";
import { resetCodeSessionRegistry } from "@/code/CodeSessionRegistry";
import { useCodeUiStore } from "@/code/CodeUiStore";
import {
  disconnectCodeUpdates,
  useCodeUpdatesStore,
} from "@/code/CodeUpdatesStore";
import { CodeWorkspacePage } from "@/code/CodeWorkspacePage";
import type { LayoutState } from "@/panel/panelTypes";
import { searchFromLayout, type PanelSearch } from "@/panel/panelUrl";
import { useUiStore } from "@/UiStore";
import {
  attentionNeedsYou,
  codeSession,
  codeWorkspace,
  harnessDoctor,
  openPrGit,
  subagentsDigest,
  watchDigest,
} from "./fixtures";

type WorkspaceScenario =
  | "active"
  | "nested"
  | "start"
  | "loading"
  | "failure";

const repo: CodeRepoSnapshot = {
  id: "repo-tidebreak",
  root_path: "/Users/sam/tidebreak",
  display_name: "tidebreak",
  default_base_ref: "main",
  branch_prefix: "thet",
  quick_actions: [],
  created_at: "2026-08-10T12:00:00.000Z",
};

const pullRequest: PullRequestDigest = {
  ...(openPrGit.pr as PullRequestDigest),
  number: 2248,
  title: "Rework the code workspace around persistent conversation",
  head_branch: "thet/ui-pane-redesign",
  base_branch: "main",
  draft: false,
  review_decision: "approved",
  mergeable: "mergeable",
  merge_state_status: "clean",
  checks_summary: "8 passing, 1 pending",
  checks: [
    { name: "desktop / focused tests", bucket: "pass" },
    { name: "desktop / storybook", bucket: "pass" },
    { name: "workspace / integration", bucket: "pending" },
  ],
};

const workspace: CodeWorkspaceSnapshot = {
  ...codeWorkspace,
  id: "ws-pane-redesign",
  repo_id: repo.id,
  title: "Reconsider the workspace pane system",
  worktree_path: "/Users/sam/tidebreak/worktrees/ui-pane-redesign",
  branch_name: "thet/ui-pane-redesign",
  base_ref: "main",
  pr: pullRequest,
};

const otherWorkspaces: CodeWorkspaceSnapshot[] = [
  {
    ...workspace,
    id: "ws-provider-errors",
    title: "Make provider failures recover gracefully",
    branch_name: "thet/provider-error-recovery",
    pr: undefined,
  },
  {
    ...workspace,
    id: "ws-release-copy",
    title: "Polish the release checklist copy",
    branch_name: "thet/release-checklist-copy",
    pr: {
      ...pullRequest,
      number: 2241,
      title: "Polish the release checklist copy",
      checks_summary: "9 passing",
      checks: [{ name: "docs / build", bucket: "pass" }],
    },
  },
];

const session: CodeSessionSnapshot = {
  ...codeSession,
  id: "sess-pane-redesign",
  workspace_id: workspace.id,
  lifecycle: "idle",
  attention: { state: { type: "done_unreviewed" }, source: "lifecycle" },
  model: "claude-opus-5",
};

const usage = {
  input_tokens: 18_420,
  output_tokens: 1_842,
  cache_read_input_tokens: 11_730,
  cache_creation_input_tokens: 0,
};

const turns: CodeTurnSnapshot[] = [
  {
    id: "turn-direction",
    session_id: session.id,
    ordinal: 1,
    status: "completed",
    user_input:
      "Take a ground-up pass at the workspace UI. Keep the conversation central and make the pane system feel effortless.",
    attachments: [],
    usage,
    diffstat: { files: 7, insertions: 284, deletions: 391, truncated: false },
    started_at: "2026-08-20T13:02:00.000Z",
    ended_at: "2026-08-20T13:11:24.000Z",
  },
  {
    id: "turn-drag",
    session_id: session.id,
    ordinal: 2,
    status: "completed",
    user_input:
      "Make drag and drop into a split pane reliable, then Storybook the important states.",
    attachments: [],
    usage,
    diffstat: { files: 4, insertions: 153, deletions: 44, truncated: false },
    started_at: "2026-08-20T13:18:00.000Z",
    ended_at: "2026-08-20T13:25:42.000Z",
  },
];

const prSnapshot: CodeWorkspacePrSnapshot = {
  ...openPrGit,
  dirty: true,
  ahead: 2,
  has_upstream: true,
  suggested_commit_message: "Redesign the code workspace pane system",
  pr: pullRequest,
};

const changedFiles = {
  files: [
    {
      path: "crates/tidebreak-desktop/ui/src/code/CodeWorkspacePage.tsx",
      kind: "modified" as const,
      insertions: 96,
      deletions: 61,
    },
    {
      path: "crates/tidebreak-desktop/ui/src/code/WorkspaceCard.tsx",
      kind: "modified" as const,
      insertions: 142,
      deletions: 301,
    },
    {
      path: "crates/tidebreak-desktop/ui/src/code/editorDrag.ts",
      kind: "added" as const,
      insertions: 57,
      deletions: 0,
    },
  ],
  truncated: false,
  stat: { files: 3, insertions: 295, deletions: 362, truncated: false },
};

const filePaths = [
  "crates/tidebreak-desktop/ui/src/code/CodeWorkspacePage.tsx",
  "crates/tidebreak-desktop/ui/src/code/WorkspaceCard.tsx",
  "crates/tidebreak-desktop/ui/src/code/CodeInspector.tsx",
  "crates/tidebreak-desktop/ui/src/code/editorDrag.ts",
  "crates/tidebreak-desktop/ui/src/stories/CodeWorkspacePage.stories.tsx",
  "docs/decisions/0052-subagents.md",
];

function pending<T>(): Promise<T> {
  return new Promise(() => {});
}

function socketAfterOpen(afterOpen?: () => void): WebSocket {
  const socket = {
    onopen: null as WebSocket["onopen"],
    onclose: null as WebSocket["onclose"],
    onerror: null as WebSocket["onerror"],
    close() {},
    addEventListener() {},
    removeEventListener() {},
  } as unknown as WebSocket;
  queueMicrotask(() => {
    socket.onopen?.(new Event("open"));
    afterOpen?.();
  });
  return socket;
}

function transcriptFrames(): SequencedCodeEventFrame[] {
  return [
    {
      seq: 1,
      replayed: true,
      event: {
        type: "session_started",
        harness_kind: "claude_code",
        harness_version: "2.1.237 (Claude Code)",
      },
    },
    {
      seq: 2,
      replayed: true,
      event: { type: "turn_started", turn_id: turns[0].id },
    },
    {
      seq: 3,
      replayed: true,
      event: {
        type: "assistant_message",
        text:
          "I’m rebuilding this around one stable center: the agent conversation. Files, changes, review, browser, and terminal become working surfaces you bring in only when they help. The PR state stays visible in the header and rail so workflow never disappears behind a pane.",
      },
    },
    {
      seq: 4,
      replayed: true,
      event: { type: "turn_completed", usage },
    },
    {
      seq: 5,
      replayed: true,
      event: { type: "turn_started", turn_id: turns[1].id },
    },
    {
      seq: 6,
      replayed: true,
      event: {
        type: "assistant_message",
        text:
          "The split interaction now carries a typed payload in DataTransfer, so the drop target can recover the dragged tab without trusting transient React state. I also enlarged the target and made its destination explicit: “Open beside the agent.”",
      },
    },
    {
      seq: 7,
      replayed: true,
      event: { type: "turn_completed", usage },
    },
  ] as SequencedCodeEventFrame[];
}

function digestFor(
  value: CodeWorkspaceSnapshot,
  overrides: Partial<CodeSessionDigest> = {},
): CodeSessionDigest {
  return {
    workspace: value.id,
    session: `sess-${value.id}`,
    kind: "interactive",
    lifecycle: "idle",
    attention: { state: { type: "done_unreviewed" }, source: "lifecycle" },
    title: value.title,
    turn_count: 2,
    ...(value.pr ? { pr_state: value.pr } : {}),
    ...overrides,
  };
}

function updateDigests(scenario: WorkspaceScenario): CodeSessionDigest[] {
  const current =
    scenario === "nested"
      ? {
          ...subagentsDigest,
          workspace: workspace.id,
          session: session.id,
          title: workspace.title,
          pr_state: pullRequest,
        }
      : digestFor(workspace, { session: session.id, pr_state: pullRequest });
  const needsYou = digestFor(otherWorkspaces[0], {
    attention: attentionNeedsYou,
    lifecycle: "idle",
    turn_count: 5,
  });
  const shipped = digestFor(otherWorkspaces[1], {
    pr_state: otherWorkspaces[1].pr,
    turn_count: 3,
  });
  const watch = {
    ...watchDigest,
    workspace: workspace.id,
    session: "sess-watch-pane-redesign",
    title: workspace.title,
    watch_state: "fixing" as const,
    watch_detail: "storybook build is still running",
    watch_cycles: 1,
  };
  return scenario === "nested"
    ? [current, watch, needsYou, shipped]
    : [current, needsYou, shipped];
}

function storyClient(scenario: WorkspaceScenario): ApiClient {
  const sessions =
    scenario === "active" || scenario === "nested" ? [session] : [];
  const loadWorkspace =
    scenario === "loading"
      ? () => pending<CodeWorkspaceSnapshot>()
      : scenario === "failure"
        ? () => Promise.reject(new Error("Could not reach this workspace."))
        : async () => workspace;

  return {
    getCodeWorkspace: loadWorkspace,
    listCodeWorkspaceSessions: async () => sessions,
    listCodeSessionTurns: async () => turns,
    listCodeApprovals: async () => [],
    openCodeEvents: (
      _sessionId: string,
      _after: number,
      onFrame: (frame: SequencedCodeEventFrame) => void,
    ) =>
      socketAfterOpen(() => {
        for (const frame of transcriptFrames()) onFrame(frame);
      }),
    getCodeRepo: async () => repo,
    listCodeRepos: async () => [repo],
    listCodeWorkspaces: async () => [workspace, ...otherWorkspaces],
    getHarnessDoctor: async () => harnessDoctor,
    listCodeHarnessModels: async (kind: CodeSessionSnapshot["harness_kind"]) => ({
      kind,
      models: [],
    }),
    openCodeUpdates: (onNotice: (notice: unknown) => void) =>
      socketAfterOpen(() =>
        onNotice({ type: "snapshot", sessions: updateDigests(scenario) }),
      ),
    getCodeWorkspacePr: async () => prSnapshot,
    refreshCodeWorkspacePr: async () => prSnapshot,
    listCodeWorkspaceTree: async () => ({ paths: filePaths, truncated: false }),
    searchCodeWorkspace: async () => ({ matches: [], truncated: false }),
    listCodeWorkspaceFiles: async () => changedFiles,
    getCodeWorkspaceDiff: async () => ({
      diff: [
        "diff --git a/src/code/editorDrag.ts b/src/code/editorDrag.ts",
        "new file mode 100644",
        "--- /dev/null",
        "+++ b/src/code/editorDrag.ts",
        "@@ -0,0 +1,4 @@",
        "+export const CODE_EDITOR_DRAG_TYPE =",
        "+  'application/x-tidebreak-editor-tab';",
      ].join("\n"),
      truncated: false,
      stat: changedFiles.stat,
    }),
    getCodeWorkspaceBlob: async (_workspaceId: string, path: string) => ({
      path,
      content: [
        "export function WorkspaceCard() {",
        "  return <article className=\"rounded-xl\">…</article>;",
        "}",
      ].join("\n"),
      truncated: false,
      binary: false,
    }),
    getCodePrComments: async () => ({
      number: pullRequest.number,
      comments: [
        {
          kind: "review",
          author: "mara",
          review_state: "approved",
          body: "The conversation-first hierarchy feels much calmer.",
          created_at: "2026-08-20T14:04:00.000Z",
        },
        {
          kind: "inline",
          author: "devon",
          body: "Keep the drop destination this explicit.",
          path: "src/code/CodeWorkspacePage.tsx",
          line: 839,
          created_at: "2026-08-20T14:12:00.000Z",
        },
      ],
    }),
    getCodeSubscriptionUsage: async () => ({
      source: "local",
      providers: [],
      diagnostics: [],
    }),
    getCodeCloneDefaults: async () => ({
      gh_found: true,
      gh_authenticated: true,
      gh_remediation: "",
    }),
    createCodeSession: async () => session,
    submitCodeTurn: async (_sessionId: string, message: string) => ({
      kind: "ran",
      turn: { ...turns[1], id: "turn-story", user_input: message },
    }),
    decideCodeApproval: async () => ({}) as never,
    setCodeAttention: async () => session,
    reapCodeSession: async () => session,
    steerCodeSession: async () => undefined,
    interruptCodeSession: async () => undefined,
    commitCodeWorkspace: async () => ({
      sha: "6cf15e2",
      message: prSnapshot.suggested_commit_message,
      stat: changedFiles.stat,
    }),
    pushCodeWorkspace: async () => ({
      branch: workspace.branch_name,
      remote: "origin",
    }),
    createCodePullRequest: async () => prSnapshot,
    mergeCodePr: async () => prSnapshot,
    startCodeWatch: async () => ({
      id: "watch-pane-redesign",
      workspace_id: workspace.id,
      session_id: "sess-watch-pane-redesign",
      pr_number: pullRequest.number,
      state: "watching",
      cycles: 0,
      created_at: "2026-08-20T14:00:00.000Z",
      updated_at: "2026-08-20T14:00:00.000Z",
    }),
    stopCodeWatch: async () => ({
      id: "watch-pane-redesign",
      workspace_id: workspace.id,
      session_id: "sess-watch-pane-redesign",
      pr_number: pullRequest.number,
      state: "stopped",
      cycles: 1,
      created_at: "2026-08-20T14:00:00.000Z",
      updated_at: "2026-08-20T14:20:00.000Z",
    }),
    runCodeWorkspaceAction: async () => ({
      name: "storybook",
      success: true,
      exit_code: 0,
      stdout: "",
      stderr: "",
      timed_out: false,
    }),
    patchCodeWorkspace: async () => workspace,
    archiveCodeWorkspace: async () => ({ ...workspace, status: "archived" }),
  } as unknown as ApiClient;
}

function appContext(client: ApiClient): AppContextValue {
  return {
    client,
    models: [],
    defaultModelKey: null,
    providers: [],
    refreshCatalog: async () => {},
    refreshChats: async () => {},
    status: "",
    setStatus: () => {},
    newChat: () => {},
    deleteChat: () => {},
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
    updateState: { status: "idle", version: null, error: null, enabled: false },
    updateUpToDate: false,
    checkForUpdate: async () => ({
      status: "idle",
      version: null,
      error: null,
      enabled: false,
    }),
    restartForUpdate: async () => {},
  };
}

function parsePanelSearch(search: Record<string, unknown>): PanelSearch {
  const text = (value: unknown) =>
    typeof value === "string" ? value : undefined;
  return {
    tabs: text(search.tabs),
    active: text(search.active),
    fullscreen: text(search.fullscreen),
    split: text(search.split),
    splitActive: text(search.splitActive),
    splitFocused: text(search.splitFocused),
    task: text(search.task),
    left: text(search.left),
    right: text(search.right),
  };
}

function storyRouter(client: ApiClient, initialUrl: string) {
  const rootRoute = createRootRoute();
  const homeRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/",
    component: () => <p className="p-6">Work</p>,
  });
  const codeRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/code",
    component: () => <p className="p-6">Code</p>,
  });
  const settingsRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/settings",
    component: () => <p className="p-6">Settings</p>,
  });
  const repoRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/code/r/$repoId",
    component: () => <p className="p-6">Repo</p>,
  });
  const workspaceRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/code/w/$workspaceId",
    validateSearch: parsePanelSearch,
    component: function WorkspaceRoute() {
      const { workspaceId } = workspaceRoute.useParams();
      return (
        <AppContextProvider value={appContext(client)}>
          <CodeWorkspacePage workspaceId={workspaceId} />
        </AppContextProvider>
      );
    },
  });
  return createRouter({
    routeTree: rootRoute.addChildren([
      homeRoute,
      codeRoute,
      settingsRoute,
      repoRoute,
      workspaceRoute,
    ]),
    history: createMemoryHistory({ initialEntries: [initialUrl] }),
  });
}

function resetStoryState(reviewOpen: boolean, sidebarCollapsed: boolean) {
  resetCodeSessionRegistry();
  disconnectCodeUpdates();
  useCodeCatalogStore.getState().reset();
  useCodeUpdatesStore.getState().reset();
  useCodeUiStore.setState({
    reviewSidebarOpen: reviewOpen,
    inspectorScope: null,
    pendingComposerPrompt: null,
    composerActionScope: null,
    newWorkspaceOpen: false,
    addRepoOpen: false,
  });
  useUiStore.setState({ sidebarCollapsed, sidebarWidth: 280 });
}

function WorkspacePageStory({
  scenario,
  initialUrl,
  reviewOpen,
  sidebarCollapsed = false,
}: {
  scenario: WorkspaceScenario;
  initialUrl: string;
  reviewOpen: boolean;
  sidebarCollapsed?: boolean;
}) {
  const [state] = useState(() => {
    resetStoryState(reviewOpen, sidebarCollapsed);
    const client = storyClient(scenario);
    return { client, router: storyRouter(client, initialUrl) };
  });

  useEffect(
    () => () => {
      resetCodeSessionRegistry();
      disconnectCodeUpdates();
      useCodeCatalogStore.getState().reset();
    },
    [],
  );

  return (
    <div className="app-shell h-full min-h-0 w-full overflow-hidden">
      <RouterProvider router={state.router as never} />
    </div>
  );
}

function workspaceUrl(layout?: LayoutState): string {
  if (!layout) return `/code/w/${workspace.id}`;
  const search = searchFromLayout(layout);
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(search)) {
    if (value !== undefined) params.set(key, value);
  }
  const query = params.toString();
  return `/code/w/${workspace.id}${query ? `?${query}` : ""}`;
}

function dragTransfer(): DataTransfer {
  const payload = new Map<string, string>();
  const transfer = {
    effectAllowed: "none",
    dropEffect: "none",
    types: [] as string[],
    setData(type: string, value: string) {
      payload.set(type, value);
      if (!transfer.types.includes(type)) transfer.types.push(type);
    },
    getData(type: string) {
      return payload.get(type) ?? "";
    },
  };
  return transfer as unknown as DataTransfer;
}

const conversationUrl = workspaceUrl();
const fileUrl = workspaceUrl({
  tabs: [
    {
      type: "file",
      path: "crates/tidebreak-desktop/ui/src/code/WorkspaceCard.tsx",
    },
  ],
  activeIndex: 0,
  fullscreen: false,
});
const splitUrl = workspaceUrl({
  tabs: [
    {
      type: "file",
      path: "crates/tidebreak-desktop/ui/src/code/WorkspaceCard.tsx",
    },
  ],
  activeIndex: 0,
  fullscreen: false,
  editorSplit: {
    tabs: [
      {
        type: "diff",
        path: "crates/tidebreak-desktop/ui/src/code/CodeWorkspacePage.tsx",
      },
    ],
    activeIndex: 0,
    focused: true,
  },
});

const meta = {
  title: "Code/Workspace page",
  component: WorkspacePageStory,
  args: {
    scenario: "active",
    initialUrl: conversationUrl,
    reviewOpen: false,
    sidebarCollapsed: false,
  },
  parameters: { layout: "fullscreen" },
  render: (args) => (
    <WorkspacePageStory
      key={`${args.scenario}:${args.initialUrl}:${args.reviewOpen}:${args.sidebarCollapsed}`}
      {...args}
    />
  ),
} satisfies Meta<typeof WorkspacePageStory>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The default: one stable conversation, with workflow state always in reach. */
export const ConversationAlone: Story = {};

/** Review is an optional working pane, not a replacement for the conversation. */
export const ConversationWithReview: Story = {
  args: { reviewOpen: true },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByRole("tab", { name: "Pull request" }),
    );
    await expect(
      await canvas.findByText(
        "Rework the code workspace around persistent conversation",
      ),
    ).toBeVisible();
  },
};

/** Two working surfaces can sit together without moving the conversation model. */
export const SplitEditorPane: Story = {
  args: { initialUrl: splitUrl, reviewOpen: false },
};

/** The full-size drop destination appears as soon as a center tab starts moving. */
export const ActiveDragTarget: Story = {
  args: { initialUrl: fileUrl, reviewOpen: false },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const tab = await canvas.findByRole("tab", { name: "WorkspaceCard.tsx" });
    const draggable = tab.closest('[draggable="true"]') ?? tab;
    fireEvent.dragStart(draggable, { dataTransfer: dragTransfer() });
    await expect(await canvas.findByTestId("split-drop-zone")).toBeVisible();
  },
};

/** Watch work and harness subagents nest under the conversation they belong to. */
export const NestedTasks: Story = {
  args: { scenario: "nested", reviewOpen: false },
};

/** A workspace with no session keeps the first prompt calm and centered. */
export const StartSession: Story = {
  args: { scenario: "start", reviewOpen: false },
};

export const Loading: Story = {
  args: { scenario: "loading", reviewOpen: false },
};

export const Failure: Story = {
  args: { scenario: "failure", reviewOpen: false },
};

/** Compact panes collapse global navigation so the conversation remains usable. */
export const CompactConversation: Story = {
  args: { sidebarCollapsed: true, reviewOpen: false },
  parameters: { viewport: { defaultViewport: "compact" } },
};
