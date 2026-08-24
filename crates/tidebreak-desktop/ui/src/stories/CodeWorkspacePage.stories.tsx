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
  CodeSubagentStatus,
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
import {
  panelSearchFrom,
  searchFromLayout,
  type PanelSearch,
} from "@/panel/panelUrl";
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

type SubagentScenario =
  | "subagent-running"
  | "subagent-completed"
  | "subagent-failed"
  | "subagent-waiting"
  | "subagent-recovered"
  | "subagent-empty";

type WorkspaceScenario =
  | "active"
  | "nested"
  | "start"
  | "loading"
  | "failure"
  | SubagentScenario;

type SubagentStorySpec = {
  callId: string;
  name: string;
  status: CodeSubagentStatus;
  includeInDigest: boolean;
};

const SUBAGENT_STORIES: Record<SubagentScenario, SubagentStorySpec> = {
  "subagent-running": {
    callId: "toolu_story_running",
    name: "Audit drag-and-drop boundaries",
    status: "running",
    includeInDigest: true,
  },
  "subagent-completed": {
    callId: "toolu_story_completed",
    name: "Review pane navigation contracts",
    status: "done",
    includeInDigest: true,
  },
  "subagent-failed": {
    callId: "toolu_story_failed",
    name: "Run the desktop integration suite",
    status: "failed",
    includeInDigest: true,
  },
  "subagent-waiting": {
    callId: "toolu_story_waiting",
    name: "Trace the recovery journal",
    status: "running",
    includeInDigest: true,
  },
  "subagent-recovered": {
    callId: "toolu_story_recovered",
    name: "Recover the interrupted audit",
    status: "failed",
    includeInDigest: false,
  },
  "subagent-empty": {
    callId: "toolu_story_empty",
    name: "Check generated bindings",
    status: "done",
    includeInDigest: true,
  },
};

function isSubagentScenario(
  scenario: WorkspaceScenario,
): scenario is SubagentScenario {
  return Object.prototype.hasOwnProperty.call(SUBAGENT_STORIES, scenario);
}

function subagentStorySpec(
  scenario: WorkspaceScenario,
): SubagentStorySpec | null {
  return isSubagentScenario(scenario) ? SUBAGENT_STORIES[scenario] : null;
}

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
  context_tokens: 24_180,
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

function transcriptFrames(
  scenario: WorkspaceScenario,
): SequencedCodeEventFrame[] {
  const frames: SequencedCodeEventFrame[] = [
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
        text: "I’m rebuilding this around one stable center: the agent conversation. Files, changes, review, browser, and terminal become working surfaces you bring in only when they help. The PR state stays visible in the header and rail so workflow never disappears behind a pane.",
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
        text: "Tabs now move on pointer events rather than native drag, so a drag starts every time in the desktop webview. Dropping one on another tab reorders the strip; dropping it on the right-hand target opens it beside the agent.",
      },
    },
    {
      seq: 7,
      replayed: true,
      event: { type: "turn_completed", usage },
    },
  ];
  const spec = subagentStorySpec(scenario);
  if (!spec) return frames;

  let seq = frames.length;
  const emit = (event: SequencedCodeEventFrame["event"]) => {
    frames.push({ seq: ++seq, replayed: true, event });
  };
  emit({ type: "turn_started", turn_id: `turn-${scenario}` });
  emit({
    type: "tool_started",
    call_id: spec.callId,
    name: "Task",
    detail: { kind: "other", summary: spec.name },
  });

  switch (scenario) {
    case "subagent-running":
      emit({
        type: "tool_started",
        call_id: `${spec.callId}-search`,
        name: "Grep",
        detail: { kind: "search", query: "CODE_EDITOR_DRAG_TYPE" },
        parent_call_id: spec.callId,
      });
      emit({
        type: "tool_completed",
        call_id: `${spec.callId}-search`,
        outcome: "succeeded",
        preview: "Found the pointer sensor wiring in the editor tab strip.",
        parent_call_id: spec.callId,
      });
      emit({
        type: "assistant_message",
        text: "The pointer sensor picks the tab up cleanly. I’m checking the focus handoff before I report back.",
        parent_call_id: spec.callId,
      });
      break;
    case "subagent-completed":
      emit({
        type: "tool_started",
        call_id: `${spec.callId}-read`,
        name: "Read",
        detail: { kind: "file_read", path: "src/panel/usePanelNav.ts" },
        parent_call_id: spec.callId,
      });
      emit({
        type: "tool_completed",
        call_id: `${spec.callId}-read`,
        outcome: "succeeded",
        preview: "The selected child address survives every layout write.",
        parent_call_id: spec.callId,
      });
      emit({
        type: "assistant_message",
        text: "The pane URL contract is stable: selecting a subagent never remounts the parent session.",
        parent_call_id: spec.callId,
      });
      emit({
        type: "tool_completed",
        call_id: spec.callId,
        outcome: "succeeded",
        preview: "Navigation contract verified.",
      });
      emit({ type: "turn_completed", usage });
      break;
    case "subagent-failed":
      emit({
        type: "tool_started",
        call_id: `${spec.callId}-command`,
        name: "Bash",
        detail: {
          kind: "command",
          cmd: "pnpm test:desktop",
          cwd: workspace.worktree_path,
        },
        parent_call_id: spec.callId,
      });
      emit({
        type: "tool_completed",
        call_id: `${spec.callId}-command`,
        outcome: "failed",
        preview: "Native browser host was unavailable.",
        parent_call_id: spec.callId,
      });
      emit({
        type: "assistant_message",
        text: "The desktop suite stopped in the native browser harness before assertions ran.",
        parent_call_id: spec.callId,
      });
      emit({
        type: "tool_completed",
        call_id: spec.callId,
        outcome: "failed",
        preview: "Integration suite could not start.",
      });
      emit({ type: "turn_completed", usage });
      break;
    case "subagent-recovered":
      emit({
        type: "tool_completed",
        call_id: spec.callId,
        outcome: "failed",
        preview: "Parent session recovered and settled stale work.",
      });
      break;
    case "subagent-empty":
      emit({
        type: "tool_completed",
        call_id: spec.callId,
        outcome: "succeeded",
        preview: "Bindings are current.",
      });
      emit({ type: "turn_completed", usage });
      break;
    case "subagent-waiting":
      break;
  }

  return frames;
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
  const selectedSubagent = subagentStorySpec(scenario);
  const current =
    scenario === "nested"
      ? {
          ...subagentsDigest,
          workspace: workspace.id,
          session: session.id,
          title: workspace.title,
          pr_state: pullRequest,
        }
      : selectedSubagent
        ? digestFor(workspace, {
            session: session.id,
            lifecycle:
              selectedSubagent.status === "running" ? "running" : "idle",
            attention:
              selectedSubagent.status === "running"
                ? { state: { type: "working" }, source: "lifecycle" }
                : {
                    state: { type: "done_unreviewed" },
                    source: "lifecycle",
                  },
            turn_count: 3,
            pr_state: pullRequest,
            ...(selectedSubagent.status === "running"
              ? { activity: "subagents" }
              : {}),
            ...(selectedSubagent.includeInDigest
              ? {
                  subagents: [
                    {
                      call_id: selectedSubagent.callId,
                      name: selectedSubagent.name,
                      status: selectedSubagent.status,
                    },
                  ],
                }
              : {}),
          })
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
    scenario === "start" || scenario === "loading" || scenario === "failure"
      ? []
      : [session];
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
        for (const frame of transcriptFrames(scenario)) onFrame(frame);
      }),
    getCodeRepo: async () => repo,
    listCodeRepos: async () => [repo],
    listCodeWorkspaces: async () => [workspace, ...otherWorkspaces],
    getHarnessDoctor: async () => harnessDoctor,
    listCodeHarnessModels: async (
      kind: CodeSessionSnapshot["harness_kind"],
    ) => ({
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
        '  return <article className="rounded-xl">…</article>;',
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
    attachment: "local",
    restartForUpdate: async () => {},
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
  const workspaceRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/code/w/$workspaceId",
    validateSearch: (search: Record<string, unknown>): PanelSearch =>
      panelSearchFrom(search),
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
      <div className="app-body">
        <RouterProvider router={state.router as never} />
      </div>
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

function subagentUrl(scenario: SubagentScenario): string {
  const spec = SUBAGENT_STORIES[scenario];
  return `${conversationUrl}?subagent=${encodeURIComponent(spec.callId)}`;
}

/**
 * Press a tab and drag it far enough to count as a drag.
 *
 * Tabs move on pointer events, so the gesture is a real press and a real move
 * rather than a synthetic `dragstart`. The move has to clear the sensor's
 * four-pixel activation distance, which is what keeps a click a click.
 */
function startTabDrag(tab: Element) {
  // The press lands on the tab itself, not the draggable wrapper around it.
  // That is the gesture that used to fail: WebKit would not begin an
  // ancestor's native drag from the inner button, so the drag never started.
  const box = tab.getBoundingClientRect();
  const from = { x: box.left + box.width / 2, y: box.top + box.height / 2 };
  fireEvent.pointerDown(tab, {
    pointerId: 1,
    isPrimary: true,
    button: 0,
    clientX: from.x,
    clientY: from.y,
  });
  fireEvent.pointerMove(document, {
    pointerId: 1,
    clientX: from.x + 40,
    clientY: from.y,
  });
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
    startTabDrag(tab);
    await expect(await canvas.findByTestId("split-drop-zone")).toBeVisible();
  },
};

/** Watch work and harness subagents nest under the conversation they belong to. */
export const NestedTasks: Story = {
  args: { scenario: "nested", reviewOpen: false },
};

/** A running child keeps its own attributed tools and text out of the parent transcript. */
export const RunningSubagent: Story = {
  args: {
    scenario: "subagent-running",
    initialUrl: subagentUrl("subagent-running"),
    reviewOpen: false,
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const context = await canvas.findByTestId("subagent-context-bar");
    await expect(context).toHaveTextContent("Audit drag-and-drop boundaries");
    await expect(context).toHaveTextContent("Running");
    await expect(
      await canvas.findByText(
        "The pointer sensor picks the tab up cleanly. I’m checking the focus handoff before I report back.",
      ),
    ).toBeVisible();
  },
};

/** Settled work stays linkable and clearly read-only after the parent turn ends. */
export const CompletedSubagent: Story = {
  args: {
    scenario: "subagent-completed",
    initialUrl: subagentUrl("subagent-completed"),
    reviewOpen: false,
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const context = await canvas.findByTestId("subagent-context-bar");
    await expect(context).toHaveTextContent("Review pane navigation contracts");
    await expect(context).toHaveTextContent("Completed");
    await expect(
      await canvas.findByText(
        "The pane URL contract is stable: selecting a subagent never remounts the parent session.",
      ),
    ).toBeVisible();
  },
};

/** Failed work keeps its useful output and a distinct terminal status. */
export const FailedSubagent: Story = {
  args: {
    scenario: "subagent-failed",
    initialUrl: subagentUrl("subagent-failed"),
    reviewOpen: false,
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const context = await canvas.findByTestId("subagent-context-bar");
    await expect(context).toHaveTextContent(
      "Run the desktop integration suite",
    );
    await expect(context).toHaveTextContent("Failed");
    await expect(
      await canvas.findByText(
        "The desktop suite stopped in the native browser harness before assertions ran.",
      ),
    ).toBeVisible();
  },
};

/** A live Task with no attributed output says that it is waiting, not empty or broken. */
export const RunningSubagentWithoutOutput: Story = {
  args: {
    scenario: "subagent-waiting",
    initialUrl: subagentUrl("subagent-waiting"),
    reviewOpen: false,
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      await canvas.findByText("Waiting for this subagent"),
    ).toBeVisible();
    await expect(
      await canvas.findByText(
        "It is still running, but it has not produced attributed transcript output yet.",
      ),
    ).toBeVisible();
  },
};

/** A bounded digest may forget an old row; journal replay recovers its name and failure. */
export const RecoveredStaleSubagent: Story = {
  args: {
    scenario: "subagent-recovered",
    initialUrl: subagentUrl("subagent-recovered"),
    reviewOpen: false,
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const context = await canvas.findByTestId("subagent-context-bar");
    await expect(context).toHaveTextContent("Recover the interrupted audit");
    await expect(context).toHaveTextContent("Failed");
    await expect(
      await canvas.findByText("No captured subagent output"),
    ).toBeVisible();
  },
};

/** Successful Tasks can legitimately finish without emitting a child transcript. */
export const EmptySubagentTranscript: Story = {
  args: {
    scenario: "subagent-empty",
    initialUrl: subagentUrl("subagent-empty"),
    reviewOpen: false,
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const context = await canvas.findByTestId("subagent-context-bar");
    await expect(context).toHaveTextContent("Check generated bindings");
    await expect(context).toHaveTextContent("Completed");
    await expect(
      await canvas.findByText("No captured subagent output"),
    ).toBeVisible();
  },
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

/** The minimum supported window keeps identity, status, and utilities distinct. */
export const MinimumWindowBusy: Story = {
  args: { scenario: "nested", reviewOpen: false, sidebarCollapsed: false },
  globals: { viewport: { value: "minimumWindow", isRotated: false } },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const header = await canvas.findByTestId("workspace-header");
    const status = within(header).getByRole("group", {
      name: "Workspace status and workflow",
    });
    const utilities = within(header).getByTestId("workspace-header-utilities");
    await expect(
      within(status).getByTestId("workspace-workflow-control"),
    ).toBeVisible();
    await expect(
      within(status).getByText(`#${pullRequest.number}`),
    ).toBeVisible();
    await expect(
      within(utilities).getByRole("button", { name: "Terminal" }),
    ).toBeVisible();
    await expect(
      within(utilities).getByRole("button", { name: "Review sidebar" }),
    ).toBeVisible();
  },
};

/** Compact panes collapse global navigation so the conversation remains usable. */
export const CompactConversation: Story = {
  args: { sidebarCollapsed: true, reviewOpen: false },
  globals: { viewport: { value: "compact", isRotated: false } },
};

/** The filtered context and transcript remain legible in the compact desktop pane. */
export const CompactSubagentTranscript: Story = {
  args: {
    scenario: "subagent-running",
    initialUrl: subagentUrl("subagent-running"),
    sidebarCollapsed: true,
    reviewOpen: false,
  },
  globals: { viewport: { value: "compact", isRotated: false } },
};
