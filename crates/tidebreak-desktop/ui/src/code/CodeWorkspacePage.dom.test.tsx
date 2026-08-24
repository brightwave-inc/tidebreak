// @vitest-environment jsdom
import type { ReactNode } from "react";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";

import { AppContextProvider, type AppContextValue } from "@/AppContext";
import type {
  CodeSessionSnapshot,
  CodeWorkspacePrSnapshot,
  CodeWorkspaceSnapshot,
  PullRequestDigest,
  SequencedCodeEventFrame,
} from "@/api/types";
import { panelSearchFrom, type PanelSearch } from "@/panel/panelUrl";
import { toast } from "sonner";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { resetCodeSessionRegistry } from "./CodeSessionRegistry";
import { useCodeUiStore } from "./CodeUiStore";
import { disconnectCodeUpdates, useCodeUpdatesStore } from "./CodeUpdatesStore";
import { CodeWorkspacePage } from "./CodeWorkspacePage";
import {
  readStoredBrowserSession,
  seedBrowserSession,
  writeStoredBrowserSession,
} from "./browser/browserPersistence";

const browserMocks = vi.hoisted(() => ({
  close: vi.fn(async () => undefined),
}));

vi.mock("./browser/browserHost", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./browser/browserHost")>();
  return { ...actual, closeCodeBrowser: browserMocks.close };
});

vi.mock("./browser/CodeBrowserTab", () => ({
  CodeBrowserTab: ({
    browserId,
    obscured,
    onTitleChange,
  }: {
    browserId: string;
    obscured?: boolean;
    onTitleChange?: (title: string) => void;
  }) => (
    <button
      type="button"
      data-testid={`browser-panel-${browserId}`}
      data-obscured={String(Boolean(obscured))}
      onClick={() => onTitleChange?.("Tidebreak docs")}
    >
      Browser panel
    </button>
  ),
}));

vi.mock("@xterm/xterm", () => ({
  Terminal: class {
    cols = 80;
    rows = 24;
    write(_data: string, cb?: () => void) {
      cb?.();
    }
    loadAddon() {}
    open() {}
    dispose() {}
    onData() {
      return { dispose() {} };
    }
  },
}));

vi.mock("@xterm/addon-fit", () => ({
  FitAddon: class {
    fit() {}
  },
}));

vi.mock("@xterm/xterm/css/xterm.css", () => ({}));

vi.mock("./FileViewer", () => ({
  FileViewer: ({ path }: { path: string }) => (
    <div data-testid="file-viewer">{path}</div>
  ),
}));

vi.mock("@monaco-editor/react", () => ({
  default: () => null,
  loader: { config() {} },
}));

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn(), message: vi.fn() },
}));

// The resize library lays out from real element measurements, which jsdom does
// not provide; left alone it registers no regions and renders nothing.
vi.mock("react-resizable-panels", () => ({
  PanelGroup: ({ children }: { children: ReactNode }) => <div>{children}</div>,
  Panel: ({ children }: { children: ReactNode }) => <div>{children}</div>,
  PanelResizeHandle: () => <div />,
}));

const WORKSPACE: CodeWorkspaceSnapshot = {
  id: "ws-1",
  repo_id: "repo-1",
  title: "Fix login",
  worktree_path: "/tmp/app/.worktrees/fix-login",
  branch_name: "tidebreak/fix-login",
  base_ref: "main",
  status: "active" as const,
  created_at: "2026-08-15T00:00:00.000Z",
};

const REPO = {
  id: "repo-1",
  root_path: "/tmp/app",
  display_name: "app",
  default_base_ref: "main",
  branch_prefix: "tidebreak",
  quick_actions: [] as {
    name: string;
    command: string;
    auto_run_on_create: boolean;
  }[],
  created_at: "2026-08-15T00:00:00.000Z",
};

const SESSION: CodeSessionSnapshot = {
  id: "sess-1",
  workspace_id: "ws-1",
  kind: "interactive" as const,
  harness_kind: "claude_code" as const,
  permission_mode: "ask" as const,
  fast_mode: false,
  lifecycle: "idle" as const,
  attention: {
    state: { type: "done_unreviewed" as const },
    source: "lifecycle" as const,
  },
  unrecognized_event_count: 0,
  created_at: "2026-08-15T00:00:00.000Z",
};

const TURN = {
  id: "turn-1",
  session_id: "sess-1",
  ordinal: 1,
  status: "completed" as const,
  user_input: "list the files",
  attachments: [],
  started_at: "2026-08-15T00:00:00.000Z",
  ended_at: "2026-08-15T00:02:14.000Z",
  diffstat: { files: 2, insertions: 42, deletions: 7, truncated: false },
  usage: {
    input_tokens: 11_000,
    output_tokens: 12,
    cache_read_input_tokens: 0,
    cache_creation_input_tokens: 0,
    // Deliberately not the sum of the four: the ring reads the prompt the
    // engine reported resident, not what the turn spent getting there.
    context_tokens: 9_500,
  },
};

const PR: PullRequestDigest = {
  number: 41,
  state: "open" as const,
  title: "Fix login flow",
  url: "https://github.com/acme/app/pull/41",
  draft: true,
  head_branch: "tidebreak/fix-login",
  base_branch: "main",
  checks: [
    { name: "ci / rust", bucket: "pass" as const },
    { name: "ci / ui", bucket: "pending" as const },
  ],
};

function codeEventSocket(
  onFrame: (frame: SequencedCodeEventFrame) => void,
  frames: readonly SequencedCodeEventFrame[] = [],
): WebSocket {
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
    for (const frame of frames) onFrame(frame);
  });
  return socket;
}

function makeClient() {
  return {
    getCodeWorkspace: vi.fn(async () => WORKSPACE),
    listCodeWorkspaceSessions: vi.fn(
      async (): Promise<(typeof SESSION)[]> => [],
    ),
    listCodeSessionTurns: vi.fn(async (_sessionId: string) => [TURN]),
    listCodeApprovals: vi.fn(async () => []),
    openCodeEvents: vi.fn(
      (
        _sessionId: string,
        _after: number,
        onFrame: (frame: SequencedCodeEventFrame) => void,
      ) => codeEventSocket(onFrame),
    ),
    getCodeRepo: vi.fn(async () => REPO),
    archiveCodeWorkspace: vi.fn(async () => ({
      ...WORKSPACE,
      status: "archived" as const,
    })),
    patchCodeWorkspace: vi.fn(async (id: string, body: { title: string }) => ({
      ...WORKSPACE,
      id,
      title: body.title,
    })),
    setCodeAttention: vi.fn(async () => SESSION),
    runCodeWorkspaceAction: vi.fn(async () => ({
      name: "lint",
      success: false,
      exit_code: 1,
      stdout: "oops",
      stderr: "failed",
      timed_out: false,
    })),
    getCodeWorkspacePr: vi.fn(
      async (): Promise<CodeWorkspacePrSnapshot> => ({
        dirty: false,
        unpushed: false,
        ahead: 0,
        has_upstream: false,
        suggested_commit_message: "",
        gh_found: true,
        gh_authenticated: true,
        remediation: "",
      }),
    ),
    markCodePrReady: vi.fn(
      async (): Promise<CodeWorkspacePrSnapshot> => ({
        dirty: false,
        unpushed: false,
        ahead: 0,
        has_upstream: true,
        suggested_commit_message: "",
        pr: { ...PR, draft: false },
        gh_found: true,
        gh_authenticated: true,
        remediation: "",
      }),
    ),
    mergeCodePr: vi.fn(
      async (): Promise<CodeWorkspacePrSnapshot> => ({
        dirty: false,
        unpushed: false,
        ahead: 0,
        has_upstream: true,
        suggested_commit_message: "",
        pr: { ...PR, draft: false, state: "merged", merged: true },
        gh_found: true,
        gh_authenticated: true,
        remediation: "",
      }),
    ),
    startCodeWatch: vi.fn(async () => ({
      id: "watch-1",
      workspace_id: "ws-1",
      session_id: "sess-watch",
      pr_number: 41,
      state: "watching" as const,
      cycles: 0,
      created_at: "2026-08-15T00:00:00.000Z",
      updated_at: "2026-08-15T00:00:00.000Z",
    })),
    stopCodeWatch: vi.fn(async () => ({
      id: "watch-1",
      workspace_id: "ws-1",
      session_id: "sess-watch",
      pr_number: 41,
      state: "stopped" as const,
      cycles: 0,
      created_at: "2026-08-15T00:00:00.000Z",
      updated_at: "2026-08-15T00:00:00.000Z",
    })),
    commitCodeWorkspace: vi.fn(async () => ({
      sha: "abc123",
      message: "Fix login",
      stat: { files: 1, insertions: 3, deletions: 1, truncated: false },
    })),
    pushCodeWorkspace: vi.fn(async () => ({
      branch: WORKSPACE.branch_name,
      remote: "origin",
    })),
    createCodePullRequest: vi.fn(async () => ({
      dirty: false,
      unpushed: false,
      ahead: 1,
      has_upstream: true,
      suggested_commit_message: "",
      pr: PR,
      gh_found: true,
      gh_authenticated: true,
      remediation: "",
    })),
    listCodeRepos: vi.fn(async () => [REPO]),
    listCodeWorkspaces: vi.fn(async () => [WORKSPACE]),
    getHarnessDoctor: vi.fn(async () => ({ harnesses: [] })),
    listCodeHarnessModels: vi.fn(async () => ({
      kind: "claude_code" as const,
      models: [],
    })),
    getCodeCloneDefaults: vi.fn(async () => ({
      gh_found: false,
      gh_remediation: "gh is not installed.",
    })),
    openCodeUpdates: vi.fn(
      () =>
        ({
          close() {},
          addEventListener() {},
          removeEventListener() {},
        }) as unknown as WebSocket,
    ),
    listCodeWorkspaceTree: vi.fn(async () => ({
      paths: [] as string[],
      truncated: false,
    })),
    listCodeWorkspaceFiles: vi.fn(async () => ({
      files: [],
      truncated: false,
      stat: { files: 0, insertions: 0, deletions: 0, truncated: false },
    })),
    getCodeWorkspaceBlob: vi.fn(async () => ({
      path: "src/lib.rs",
      content: "fn main() {}",
      truncated: false,
      binary: false,
    })),
    submitCodeTurn: vi.fn(async (_sessionId: string, message: string) => ({
      kind: "ran" as const,
      turn: {
        ...TURN,
        id: `turn-${message.length}`,
        ordinal: 2,
        user_input: message,
      },
    })),
    getCodeWorkspaceDiff: vi.fn(async () => ({
      diff: "",
      truncated: false,
      stat: { files: 0, insertions: 0, deletions: 0, truncated: false },
    })),
    listCodeTerminals: vi.fn(async () => []),
    createCodeTerminal: vi.fn(async () => ({
      id: "term-1",
      workspace_id: "ws-1",
      cols: 80,
      rows: 24,
      ended: false,
      created_at: "2026-08-15T00:00:00.000Z",
    })),
    readCodeTerminal: vi.fn(async () => ({
      id: "term-1",
      workspace_id: "ws-1",
      bytes: "",
      cursor: 0,
      overflow: false,
      truncated: false,
      ended: false,
    })),
    writeCodeTerminal: vi.fn(async () => undefined),
    resizeCodeTerminal: vi.fn(async () => ({
      id: "term-1",
      workspace_id: "ws-1",
      cols: 80,
      rows: 24,
      ended: false,
      created_at: "2026-08-15T00:00:00.000Z",
    })),
  };
}

function appContext(client: ReturnType<typeof makeClient>): AppContextValue {
  return {
    client: client as never,
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

/**
 * Press `element` and drag it `distance` pixels to the right.
 *
 * Pointer events, deliberately: dnd-kit's sensor listens for them and never
 * touches the native drag path, so this is the same sequence the webview
 * delivers. The move has to clear the sensor's 4px activation distance, and it
 * goes to the document because that is where the sensor listens once a press
 * has started.
 */
function dragBy(element: Element, distance: number) {
  fireEvent.pointerDown(element, {
    isPrimary: true,
    button: 0,
    pointerId: 1,
    clientX: 0,
    clientY: 0,
  });
  fireEvent.pointerMove(document, {
    pointerId: 1,
    clientX: distance,
    clientY: 0,
  });
}

/**
 * The workspace route renders the page under test; the repo route renders a
 * marker instead, so an assertion can tell the two apart the way the running
 * app does.
 */
async function mountWorkspace(
  client: ReturnType<typeof makeClient>,
  initialUrl = "/code/w/ws-1",
) {
  const rootRoute = createRootRoute();
  const codeRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/code",
    component: () => <p>code index</p>,
  });
  const codeWorkspaceRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/code/w/$workspaceId",
    validateSearch: (search: Record<string, unknown>): PanelSearch =>
      panelSearchFrom(search),
    component: function WorkspaceRoute() {
      const { workspaceId } = codeWorkspaceRoute.useParams();
      return (
        <AppContextProvider value={appContext(client)}>
          <CodeWorkspacePage workspaceId={workspaceId} />
        </AppContextProvider>
      );
    },
  });

  const router = createRouter({
    routeTree: rootRoute.addChildren([codeRoute, codeWorkspaceRoute]),
    history: createMemoryHistory({ initialEntries: [initialUrl] }),
  });
  await router.load();
  const result = render(<RouterProvider router={router as never} />);
  return { ...result, router };
}

afterEach(() => {
  cleanup();
  resetCodeSessionRegistry();
  useCodeCatalogStore.getState().reset();
  disconnectCodeUpdates();
  useCodeUpdatesStore.getState().reset();
  useCodeUiStore.setState({ reviewSidebarOpen: true, inspectorScope: null });
  useCodeUiStore.setState({
    pendingComposerPrompt: null,
    composerActionScope: null,
    workflowShortcutPending: null,
    archivePending: false,
  });
  browserMocks.close.mockClear();
  window.localStorage.clear();
  vi.restoreAllMocks();
});

describe("CodeWorkspacePage", () => {
  it("gives the transcript chat's scrolling frame and closes the turn it hydrated", async () => {
    const client = makeClient();
    client.listCodeWorkspaceSessions.mockResolvedValue([SESSION]);
    const { container } = await mountWorkspace(client);

    expect(
      await screen.findByRole("article", { name: "You" }),
    ).toHaveTextContent("list the files");

    const view = container.querySelector(".message-view");
    expect(view).not.toBeNull();
    // The transcript, not the panel slot, is the scroller: the pane claims its
    // own height, so `.messages` is what overflows.
    expect(view?.querySelector(".messages > .messages-column")).not.toBeNull();

    const pane = view?.closest(".chat-pane");
    expect(pane).not.toBeNull();
    expect(view?.parentElement?.className).toMatch(/flex/);
    // The queue tray's slot sits between the transcript and the composer
    // (decision 67); the composer stays next in the same column.
    expect(view?.nextElementSibling?.nextElementSibling).toContainElement(
      screen.getByRole("button", { name: "Send message" }),
    );

    const seam = await screen.findByRole("group", { name: "Turn finished" });
    expect(seam).toHaveTextContent("2m 14s");
    expect(seam).toHaveTextContent("2 files +42 −7");
    expect(seam).not.toHaveTextContent("in /");
    expect(
      screen.getByRole("button", { name: /Context: 9,500 tokens used/ }),
    ).toBeInTheDocument();
  });

  it("pins the composer to the bottom of a full-height column on an empty start", async () => {
    const client = makeClient();
    const { container } = await mountWorkspace(client);

    expect(
      await screen.findByText("Start a session on this workspace."),
    ).toBeInTheDocument();

    const pane = container.querySelector(".chat-pane");
    expect(pane).not.toBeNull();
    expect(pane).toContainElement(
      screen.getByRole("button", { name: "Send message" }),
    );
    // The start prompt is a flex child of the pane so `mt-auto` on the
    // composer can consume the empty region under the header.
    expect(pane?.firstElementChild?.className).toMatch(/flex-1/);
  });

  it("shows header skeleton bars instead of Workspace and a repo UUID", async () => {
    const client = makeClient();
    client.getCodeWorkspace.mockImplementation(() => new Promise(() => {}));
    await mountWorkspace(client);

    expect(screen.getByTestId("workspace-header-skeleton")).toBeInTheDocument();
    expect(screen.queryByText("Workspace")).not.toBeInTheDocument();
    expect(screen.queryByText("repo-1")).not.toBeInTheDocument();
  });

  it("marks recorded unrecognized engine events with a warning dot", async () => {
    const client = makeClient();
    client.listCodeWorkspaceSessions.mockResolvedValue([
      { ...SESSION, unrecognized_event_count: 3 },
    ]);
    await mountWorkspace(client);

    expect(
      await screen.findByRole("heading", { name: /Fix login/ }),
    ).toBeInTheDocument();
    expect(screen.getByTestId("unrecognized-event-dot")).toHaveAttribute(
      "aria-label",
      "3 unrecognized engine events recorded in this session",
    );
  });

  it("names monitor activity precisely in the workspace header", async () => {
    const client = makeClient();
    client.listCodeWorkspaceSessions.mockResolvedValue([SESSION]);
    await mountWorkspace(client);
    useCodeUpdatesStore.getState().apply({
      type: "digest",
      digest: {
        workspace: WORKSPACE.id,
        session: SESSION.id,
        kind: "interactive",
        lifecycle: "running",
        attention: { state: { type: "working" }, source: "lifecycle" },
        title: WORKSPACE.title,
        turn_count: 2,
        activity: "monitor",
      },
    });

    const utilities = await screen.findByTestId("workspace-header-utilities");
    await waitFor(() =>
      expect(within(utilities).getByText("Monitoring")).toBeInTheDocument(),
    );
    expect(within(utilities).queryByText("Running")).not.toBeInTheDocument();
  });

  it("keeps the Codex session model selectable before its catalog loads", async () => {
    const client = makeClient();
    client.listCodeWorkspaceSessions.mockResolvedValue([
      {
        ...SESSION,
        harness_kind: "codex",
        model: "gpt-5.6-luna",
      },
    ]);
    client.listCodeHarnessModels.mockResolvedValue({
      kind: "codex",
      models: [],
    } as never);
    await mountWorkspace(client);

    const model = await screen.findByRole("button", {
      name: "Model: GPT 5.6 Luna",
    });
    expect(model).toBeEnabled();
    expect(model).toHaveAttribute("title", "Model: GPT 5.6 Luna");
  });

  it.each([
    {
      harness: "codex" as const,
      current: "gpt-5.6-luna",
      currentLabel: "GPT 5.6 Luna",
      next: "gpt-5.6-sol",
      nextLabel: "GPT 5.6 Sol",
    },
    {
      harness: "opencode" as const,
      current: "model-gateway/kimi-k3",
      currentLabel: "Kimi K3",
      next: "model-gateway/deepseek-v4-pro",
      nextLabel: "DeepSeek V4 Pro",
    },
  ])("switches models on $harness for the next turn", async (choice) => {
    const client = makeClient();
    client.listCodeWorkspaceSessions.mockResolvedValue([
      {
        ...SESSION,
        harness_kind: choice.harness,
        model: choice.current,
      },
    ]);
    client.listCodeHarnessModels.mockResolvedValue({
      kind: choice.harness,
      models: [
        {
          id: choice.current,
          label: choice.currentLabel,
          default: true,
          reasoning_efforts: [],
        },
        {
          id: choice.next,
          label: choice.nextLabel,
          default: false,
          reasoning_efforts: [],
        },
      ],
      reasoning_efforts: [],
    } as never);
    const user = userEvent.setup();
    await mountWorkspace(client);

    await user.click(
      await screen.findByRole("button", {
        name: `Model: ${choice.currentLabel}`,
      }),
    );
    await user.click(
      screen.getByRole("menuitem", { name: new RegExp(choice.nextLabel) }),
    );
    await user.type(screen.getByRole("textbox", { name: "Message" }), "go");
    await user.click(screen.getByRole("button", { name: "Send message" }));

    await waitFor(() =>
      expect(client.submitCodeTurn).toHaveBeenCalledWith(
        "sess-1",
        "go",
        choice.next,
        undefined,
      ),
    );
  });

  it("toggles the review sidebar from the header control", async () => {
    const client = makeClient();
    const user = userEvent.setup();
    await mountWorkspace(client);

    expect(
      await screen.findByRole("heading", { name: /Fix login/ }),
    ).toBeInTheDocument();
    expect(screen.getByTestId("code-inspector")).toBeInTheDocument();
    expect(useCodeUiStore.getState().reviewSidebarOpen).toBe(true);

    await user.click(screen.getByRole("button", { name: "Review sidebar" }));

    expect(useCodeUiStore.getState().reviewSidebarOpen).toBe(false);
    expect(screen.queryByTestId("code-inspector")).not.toBeInTheDocument();
  });

  it("surfaces a quick-action exit code on the result toast", async () => {
    const client = makeClient();
    client.getCodeRepo.mockResolvedValue({
      ...REPO,
      quick_actions: [
        { name: "lint", command: "pnpm lint", auto_run_on_create: false },
      ],
    });
    const user = userEvent.setup();
    await mountWorkspace(client);

    expect(
      await screen.findByRole("heading", { name: /Fix login/ }),
    ).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Workspace actions" }));
    expect(await screen.findByRole("menu")).toHaveTextContent("app");
    await user.click(
      await screen.findByRole("menuitem", { name: "Run: lint" }),
    );

    await waitFor(() =>
      expect(client.runCodeWorkspaceAction).toHaveBeenCalledWith(
        "ws-1",
        "lint",
      ),
    );
    expect(toast.error).toHaveBeenCalledWith(
      "lint exited 1",
      expect.objectContaining({
        action: expect.objectContaining({ label: "View output" }),
      }),
    );
  });

  it("puts compact PR status and quick commands inside the header utilities", async () => {
    const client = makeClient();
    client.listCodeWorkspaceSessions.mockResolvedValue([SESSION]);
    client.getCodeWorkspace.mockResolvedValue({ ...WORKSPACE, pr: PR });
    client.getCodeWorkspacePr.mockResolvedValue({
      dirty: false,
      unpushed: false,
      ahead: 1,
      has_upstream: true,
      suggested_commit_message: "",
      pr: PR,
      gh_found: true,
      gh_authenticated: true,
      remediation: "",
    });
    const user = userEvent.setup();
    await mountWorkspace(client);

    const control = await screen.findByTestId("workspace-workflow-control");
    const utilities = screen.getByTestId("workspace-header-utilities");
    expect(control.closest("header")).not.toBeNull();
    expect(control.parentElement).toBe(utilities);
    expect(utilities.firstElementChild).toBe(control);
    expect(control).toHaveTextContent("#41");
    expect(control).toHaveTextContent("Draft");
    expect(
      within(control).getByRole("button", { name: "Mark ready" }),
    ).toBeInTheDocument();

    await user.click(
      within(control).getByRole("button", {
        name: "Workspace status: #41 · Draft",
      }),
    );
    const popover = await screen.findByTestId("workspace-workflow-popover");
    expect(popover).toHaveTextContent("tidebreak/fix-login");
    expect(popover).toHaveTextContent("1 passing · 1 pending");

    await user.click(
      within(control).getByRole("button", { name: "More workspace actions" }),
    );
    await user.click(
      await screen.findByRole("menuitem", { name: "Watch and fix" }),
    );
    // Watch and fix is a durable server-side task, not a composer prompt.
    await waitFor(() =>
      expect(client.startCodeWatch).toHaveBeenCalledWith("ws-1"),
    );

    // Readying a draft is a pull-request state change, so the button calls the
    // endpoint rather than composing a prompt, and the adopted snapshot moves
    // the control off draft. It goes last for that reason. `prActions` covers
    // what the remaining prompt-backed actions put in front of the agent.
    await user.click(
      within(control).getByRole("button", { name: "Mark ready" }),
    );
    await waitFor(() =>
      expect(client.markCodePrReady).toHaveBeenCalledWith("ws-1"),
    );
    await waitFor(() => expect(control).not.toHaveTextContent("Draft"));
    expect(client.submitCodeTurn).not.toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: "Workspace actions" }));
    const menu = await screen.findByRole("menu");
    expect(menu).toHaveTextContent("app");
    expect(menu).toHaveTextContent(WORKSPACE.worktree_path);
  });

  it("runs Ship chords through the same actions the header buttons do", async () => {
    // The shell's keymap sits above the route, so a chord arrives as a request
    // on the store and the header resolves it against the branch and pull
    // request state only it holds. This is that seam: raise the chord, and the
    // right server call has to follow with no button in between.
    const client = makeClient();
    client.listCodeWorkspaceSessions.mockResolvedValue([SESSION]);
    client.getCodeWorkspace.mockResolvedValue({ ...WORKSPACE, pr: PR });
    client.getCodeWorkspacePr.mockResolvedValue({
      dirty: false,
      unpushed: false,
      ahead: 1,
      has_upstream: true,
      suggested_commit_message: "",
      pr: PR,
      gh_found: true,
      gh_authenticated: true,
      remediation: "",
    });
    await mountWorkspace(client);
    await screen.findByTestId("workspace-workflow-control");

    act(() =>
      useCodeUiStore.getState().requestWorkflowShortcut("ws-1", "watch"),
    );
    await waitFor(() =>
      expect(client.startCodeWatch).toHaveBeenCalledWith("ws-1"),
    );

    // This one is a draft, so it is not green. The chord says so rather than
    // asking an agent to merge it — decision 42 keeps merging off the agent
    // path, so "not mergeable" has to be a refusal, not a prompt.
    act(() =>
      useCodeUiStore.getState().requestWorkflowShortcut("ws-1", "merge"),
    );
    await waitFor(() =>
      expect(toast.message).toHaveBeenCalledWith(
        "Mark the pull request ready for review on GitHub before merging it.",
      ),
    );
    expect(client.mergeCodePr).not.toHaveBeenCalled();
    expect(client.submitCodeTurn).not.toHaveBeenCalled();
  });

  it("merges a green pull request through the user endpoint, not the agent", async () => {
    // Decision 42: the general `gh` runner refuses merge argv, and only
    // POST /pr/merge reaches the runner that allows it. A chord that prompted
    // an agent to merge would route around that boundary using the agent's own
    // shell, so this pins that the chord calls the endpoint instead.
    const green: PullRequestDigest = {
      ...PR,
      draft: false,
      mergeable: "mergeable",
      merge_state_status: "clean",
      checks: [{ name: "ci / rust", bucket: "pass" as const }],
    };
    const client = makeClient();
    client.listCodeWorkspaceSessions.mockResolvedValue([SESSION]);
    client.getCodeWorkspace.mockResolvedValue({ ...WORKSPACE, pr: green });
    client.getCodeWorkspacePr.mockResolvedValue({
      dirty: false,
      unpushed: false,
      ahead: 0,
      has_upstream: true,
      suggested_commit_message: "",
      pr: green,
      gh_found: true,
      gh_authenticated: true,
      remediation: "",
    });
    const user = userEvent.setup();
    await mountWorkspace(client);
    const control = await screen.findByTestId("workspace-workflow-control");
    await waitFor(() => expect(control).toHaveTextContent("Ready"));

    act(() =>
      useCodeUiStore.getState().requestWorkflowShortcut("ws-1", "merge"),
    );

    // Merging publishes to a shared branch, so the chord still stops at the
    // same confirmation the review sidebar's Merge button shows.
    const confirmation = await screen.findByRole("alertdialog");
    expect(confirmation).toHaveTextContent("Merge #41?");
    expect(confirmation).toHaveTextContent("squash-merged into main");
    await user.click(
      within(confirmation).getByRole("button", { name: "Merge" }),
    );

    await waitFor(() =>
      expect(client.mergeCodePr).toHaveBeenCalledWith("ws-1", {
        method: "squash",
        auto: false,
      }),
    );
    expect(client.submitCodeTurn).not.toHaveBeenCalled();
  });

  it("says why a Ship chord did nothing instead of swallowing it", async () => {
    // A chord that silently no-ops is indistinguishable from one that never
    // fired, which is the fastest way to stop trusting the keyboard.
    const client = makeClient();
    client.getCodeWorkspace.mockResolvedValue(WORKSPACE);
    client.getCodeWorkspacePr.mockResolvedValue({
      dirty: false,
      unpushed: false,
      ahead: 0,
      has_upstream: true,
      suggested_commit_message: "",
      gh_found: true,
      gh_authenticated: true,
      remediation: "",
    });
    await mountWorkspace(client);
    const control = await screen.findByTestId("workspace-workflow-control");
    await waitFor(() => expect(control).toHaveTextContent("No changes"));

    act(() =>
      useCodeUiStore.getState().requestWorkflowShortcut("ws-1", "merge"),
    );
    await waitFor(() =>
      expect(toast.message).toHaveBeenCalledWith("No pull request yet"),
    );
    expect(client.submitCodeTurn).not.toHaveBeenCalled();

    // A chord raised on another workspace — an archived one, which draws no
    // header control to take it — must not fire on whatever is open here.
    act(() =>
      useCodeUiStore.getState().requestWorkflowShortcut("ws-other", "merge"),
    );
    await waitFor(() =>
      expect(useCodeUiStore.getState().workflowShortcutPending).not.toBeNull(),
    );
    expect(toast.message).toHaveBeenCalledTimes(1);
  });

  it("marks a draft ready through the endpoint rather than by prompt", async () => {
    // Readying a draft puts work in front of reviewers, which decision 42
    // keeps off the agent path for the same reason merging is. The chord for
    // "run the next step" is what reaches it at this stage.
    const client = makeClient();
    client.listCodeWorkspaceSessions.mockResolvedValue([SESSION]);
    client.getCodeWorkspace.mockResolvedValue({ ...WORKSPACE, pr: PR });
    client.getCodeWorkspacePr.mockResolvedValue({
      dirty: false,
      unpushed: false,
      ahead: 0,
      has_upstream: true,
      suggested_commit_message: "",
      pr: PR,
      gh_found: true,
      gh_authenticated: true,
      remediation: "",
    });
    await mountWorkspace(client);
    const control = await screen.findByTestId("workspace-workflow-control");
    await waitFor(() => expect(control).toHaveTextContent("Draft"));

    act(() =>
      useCodeUiStore.getState().requestWorkflowShortcut("ws-1", "next"),
    );

    await waitFor(() =>
      expect(client.markCodePrReady).toHaveBeenCalledWith("ws-1"),
    );
    expect(client.submitCodeTurn).not.toHaveBeenCalled();
  });

  it("arms auto-merge when the pull request is landable but not yet green", async () => {
    // One chord, one intent: the reader means "get this in" whether or not the
    // checks have finished. The confirmation is what names which of the two is
    // about to happen.
    const pending: PullRequestDigest = {
      ...PR,
      draft: false,
      mergeable: "mergeable",
      merge_state_status: "blocked",
      checks: [{ name: "ci / rust", bucket: "pending" as const }],
    };
    const client = makeClient();
    client.listCodeWorkspaceSessions.mockResolvedValue([SESSION]);
    client.getCodeWorkspace.mockResolvedValue({ ...WORKSPACE, pr: pending });
    client.getCodeWorkspacePr.mockResolvedValue({
      dirty: false,
      unpushed: false,
      ahead: 0,
      has_upstream: true,
      suggested_commit_message: "",
      pr: pending,
      gh_found: true,
      gh_authenticated: true,
      remediation: "",
    });
    const user = userEvent.setup();
    await mountWorkspace(client);
    await screen.findByTestId("workspace-workflow-control");

    act(() =>
      useCodeUiStore.getState().requestWorkflowShortcut("ws-1", "merge"),
    );

    const confirmation = await screen.findByRole("alertdialog");
    expect(confirmation).toHaveTextContent("Auto-merge #41?");
    expect(confirmation).toHaveTextContent(
      "once the remaining requirements pass",
    );
    await user.click(
      within(confirmation).getByRole("button", { name: "Enable auto-merge" }),
    );

    await waitFor(() =>
      expect(client.mergeCodePr).toHaveBeenCalledWith("ws-1", {
        method: "squash",
        auto: true,
      }),
    );
    expect(client.submitCodeTurn).not.toHaveBeenCalled();
  });

  it("archives from a chord through the same confirmation the menu uses", async () => {
    // Archiving removes a worktree. The keyboard path must not be the one that
    // skips the dialog.
    const client = makeClient();
    const { router } = await mountWorkspace(client);
    const user = userEvent.setup();
    await screen.findByRole("heading", { name: /Fix login/ });

    act(() => useCodeUiStore.getState().requestArchiveWorkspace());
    const confirmation = await screen.findByRole("alertdialog");
    await user.click(
      within(confirmation).getByRole("button", { name: "Archive" }),
    );

    await waitFor(() => expect(router.state.location.pathname).toBe("/code"));
    expect(client.archiveCodeWorkspace).toHaveBeenCalledWith("ws-1", false);
  });

  it("drafts the Create PR request into the composer for local changes", async () => {
    const client = makeClient();
    client.getCodeWorkspacePr.mockResolvedValue({
      dirty: true,
      unpushed: false,
      ahead: 0,
      has_upstream: true,
      suggested_commit_message: "improve login flow",
      gh_found: true,
      gh_authenticated: true,
      remediation: "",
    });
    const user = userEvent.setup();
    await mountWorkspace(client);

    const control = await screen.findByTestId("workspace-workflow-control");
    await waitFor(() =>
      expect(control).toHaveTextContent("Uncommitted changes"),
    );
    await user.click(
      within(control).getByRole("button", { name: "Create PR" }),
    );

    // Offered, not sent: the request lands as an editable draft and no turn
    // starts until the reader submits it.
    const composer = await waitFor(() => {
      const input = document.querySelector<HTMLTextAreaElement>(
        "[data-composer-input]",
      );
      expect(input?.value).toContain("open a pull request against `main`");
      return input!;
    });
    expect(composer.value).toContain("Do not merge.");
    expect(client.submitCodeTurn).not.toHaveBeenCalled();

    // Hand review is one step away: the menu still opens Source control, and
    // the suggested commit message still does not crowd the composer — the
    // draft stays exactly what Create PR put there.
    await user.click(
      within(control).getByRole("button", { name: "More workspace actions" }),
    );
    await user.click(
      await screen.findByRole("menuitem", { name: "Review & commit" }),
    );
    expect(screen.getByRole("tab", { name: "Source control" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(composer.value).not.toContain("improve login flow");

    await user.click(screen.getByRole("tab", { name: "Files" }));
    await user.click(screen.getByRole("button", { name: "Review sidebar" }));
    await user.click(screen.getByRole("button", { name: "Review sidebar" }));
    expect(screen.getByRole("tab", { name: "Files" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
  });

  it("leaves the archived workspace for its repo", async () => {
    const client = makeClient();
    const { router } = await mountWorkspace(client);
    const user = userEvent.setup();

    expect(
      await screen.findByRole("heading", { name: /Fix login/ }),
    ).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Workspace actions" }));
    await user.click(await screen.findByRole("menuitem", { name: "Archive" }));
    const confirmation = await screen.findByRole("alertdialog");
    await user.click(
      within(confirmation).getByRole("button", { name: "Archive" }),
    );

    await waitFor(() => expect(router.state.location.pathname).toBe("/code"));
    expect(client.archiveCodeWorkspace).toHaveBeenCalledWith("ws-1", false);
    expect(
      screen.queryByRole("heading", { name: /Fix login/ }),
    ).not.toBeInTheDocument();
  });

  it("force-archives with one confirm and force=true from the header menu", async () => {
    const client = makeClient();
    const { router } = await mountWorkspace(client);
    const user = userEvent.setup();

    expect(
      await screen.findByRole("heading", { name: /Fix login/ }),
    ).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Workspace actions" }));
    await user.click(
      await screen.findByRole("menuitem", {
        name: "Force archive (discard changes)",
      }),
    );
    const confirmation = await screen.findByRole("alertdialog");
    expect(confirmation).toHaveTextContent("Discard changes and archive?");
    await user.click(
      within(confirmation).getByRole("button", { name: "Discard and archive" }),
    );

    await waitFor(() => expect(router.state.location.pathname).toBe("/code"));
    expect(client.archiveCodeWorkspace).toHaveBeenCalledWith("ws-1", true);
  });

  it("keeps git and comments in the review sidebar, and opens the terminal as a tab", async () => {
    const client = makeClient();
    const { router } = await mountWorkspace(client);
    const user = userEvent.setup();

    expect(
      await screen.findByRole("heading", { name: /Fix login/ }),
    ).toBeInTheDocument();

    const inspector = screen.getByTestId("code-inspector");
    expect(
      within(inspector).getByRole("tab", { name: "Files" }),
    ).toBeInTheDocument();
    expect(
      within(inspector).getByRole("tab", { name: "Pull request" }),
    ).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Terminal" }));

    // The chord and the header button both start a shell and give it a tab,
    // named after the shell the server handed back.
    expect(
      await screen.findByRole("tab", { name: /Terminal 1/ }),
    ).toBeInTheDocument();
    expect(screen.queryByTestId("terminal-drawer")).not.toBeInTheDocument();
    await waitFor(() =>
      expect(router.state.location.search).toMatchObject({
        tabs: "terminal.term-1",
      }),
    );

    await user.click(screen.getByRole("tab", { name: "Pull request" }));
    expect(
      within(inspector).getByText("No pull request yet"),
    ).toBeInTheDocument();
  });

  it("does not promote a stale files catalog into the conversation strip", async () => {
    const client = makeClient();
    const { router } = await mountWorkspace(
      client,
      "/code/w/ws-1?tabs=files,terminal",
    );

    expect(
      await screen.findByRole("heading", { name: /Fix login/ }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("tablist", { name: "Open panels" }),
    ).not.toBeInTheDocument();
    // A link written before terminals were tabs still opens one; the pane
    // adopts a shell and the tab takes its id.
    expect(screen.getByRole("tab", { name: /Terminal 1/ })).toBeInTheDocument();
    expect(screen.queryByTestId("terminal-drawer")).not.toBeInTheDocument();
    // Adopting a shell rewrites the address to name it, so the link heals
    // itself the first time it is opened.
    await waitFor(() =>
      expect(router.state.location.search).toMatchObject({
        tabs: "terminal.term-1",
      }),
    );

    const inspector = screen.getByTestId("code-inspector");
    expect(
      within(inspector).getByRole("tab", { name: "Files" }),
    ).toBeInTheDocument();
  });

  it("opens a terminal tab without creating a files or diff strip tab", async () => {
    const client = makeClient();
    const { router } = await mountWorkspace(client);
    const user = userEvent.setup();

    expect(
      await screen.findByRole("heading", { name: /Fix login/ }),
    ).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Terminal" }));

    await waitFor(() =>
      expect(router.state.location.search).toMatchObject({
        tabs: "terminal.term-1",
      }),
    );
    expect(
      screen.queryByRole("tablist", { name: "Open panels" }),
    ).not.toBeInTheDocument();
    expect(screen.queryByRole("tab", { name: "Diff" })).not.toBeInTheDocument();
    expect(screen.getByRole("tab", { name: /Terminal 1/ })).toBeInTheDocument();
    expect(
      within(screen.getByTestId("code-inspector")).getByRole("tab", {
        name: "Files",
      }),
    ).toBeInTheDocument();
  });

  it("opens a turn diff as a center tab from the review seam", async () => {
    const client = makeClient();
    client.listCodeWorkspaceSessions.mockResolvedValue([SESSION]);
    const user = userEvent.setup();
    await mountWorkspace(client);

    await user.click(
      await screen.findByRole("button", { name: "Review this turn's changes" }),
    );

    expect(
      await screen.findByRole("tab", { name: "Main agent" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "Turn diff" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    await waitFor(() =>
      expect(client.getCodeWorkspaceDiff).toHaveBeenCalledWith("ws-1", {
        turn: "turn-1",
        file: undefined,
      }),
    );
  });

  it("moves between center tabs with the arrows and names the panel each opens", async () => {
    const client = makeClient();
    client.listCodeWorkspaceSessions.mockResolvedValue([SESSION]);
    const user = userEvent.setup();
    await mountWorkspace(client);

    await user.click(
      await screen.findByRole("button", { name: "Review this turn's changes" }),
    );

    const diff = await screen.findByRole("tab", { name: "Turn diff" });
    const chat = screen.getByRole("tab", { name: "Main agent" });
    // One tab stop for the strip, and the panel a tab opens says which tab
    // named it — otherwise the content below is orphaned from the control.
    expect(diff).toHaveAttribute("tabindex", "0");
    expect(chat).toHaveAttribute("tabindex", "-1");
    const panel = document.getElementById(
      diff.getAttribute("aria-controls") ?? "",
    );
    expect(panel).not.toBeNull();
    expect(panel).toHaveAttribute("role", "tabpanel");
    expect(panel).toHaveAttribute("aria-labelledby", diff.id);

    diff.focus();
    await user.keyboard("{ArrowLeft}");
    expect(chat).toHaveAttribute("aria-selected", "true");
    expect(chat).toHaveFocus();
    const chatPanel = document.getElementById(
      chat.getAttribute("aria-controls") ?? "",
    );
    expect(chatPanel).toHaveAttribute("aria-labelledby", chat.id);
  });

  it("offers useful right-click actions for center tabs", async () => {
    const client = makeClient();
    const user = userEvent.setup();
    const { router } = await mountWorkspace(
      client,
      "/code/w/ws-1?tabs=file.src%252Flib.rs,diff.f.src%252Fmain.rs&active=diff.f.src%252Fmain.rs",
    );

    const fileTab = await screen.findByRole("tab", { name: "lib.rs" });
    expect(screen.getByRole("tab", { name: "main.rs (diff)" })).toHaveAttribute(
      "aria-selected",
      "true",
    );

    fireEvent.contextMenu(fileTab);
    const menu = await screen.findByRole("menu");
    expect(within(menu).getByText("lib.rs")).toBeInTheDocument();
    expect(
      screen.getByRole("menuitem", { name: "Copy path" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("menuitem", { name: "Close tab" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("menuitem", { name: "Close other tabs" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("menuitem", { name: "Close tabs to the right" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("menuitem", { name: "Close all tabs" }),
    ).toBeInTheDocument();

    await user.click(screen.getByRole("menuitem", { name: "Copy path" }));
    await waitFor(async () =>
      expect(await window.navigator.clipboard.readText()).toBe("src/lib.rs"),
    );
    expect(toast.success).toHaveBeenCalledWith("Copied path");

    fireEvent.contextMenu(fileTab);
    await user.click(
      await screen.findByRole("menuitem", {
        name: "Close tabs to the right",
      }),
    );
    expect(
      screen.queryByRole("tab", { name: "main.rs (diff)" }),
    ).not.toBeInTheDocument();
    await waitFor(() =>
      expect(router.state.location.search).toMatchObject({
        tabs: "file.src%2Flib.rs",
      }),
    );

    const chatTab = screen.getByRole("tab", { name: "Main agent" });
    fireEvent.contextMenu(chatTab);
    await user.click(
      await screen.findByRole("menuitem", { name: "Close other tabs" }),
    );
    expect(
      screen.getByRole("tablist", { name: "Workspace center" }),
    ).toBeInTheDocument();
    expect(chatTab).toHaveAttribute("aria-selected", "true");
  });

  it("gives every agent in the workspace its own tab and switches between them", async () => {
    const client = makeClient();
    const second: CodeSessionSnapshot = {
      ...SESSION,
      id: "sess-2",
      harness_kind: "codex",
      created_at: "2026-08-15T01:00:00.000Z",
    };
    // The list arrives newest first; the strip reads oldest first, so the
    // agent the workspace started with keeps the left-hand slot.
    client.listCodeWorkspaceSessions.mockResolvedValue([second, SESSION]);
    client.listCodeSessionTurns.mockImplementation(async (sessionId: string) =>
      sessionId === "sess-1"
        ? [TURN]
        : [
            {
              ...TURN,
              id: "turn-2",
              session_id: "sess-2",
              user_input: "run the tests",
            },
          ],
    );
    const user = userEvent.setup();
    const { router } = await mountWorkspace(client);

    const main = await screen.findByRole("tab", { name: "Main agent" });
    const codex = screen.getByRole("tab", { name: "Codex CLI" });
    expect(main).toHaveAttribute("aria-selected", "true");
    expect(
      await screen.findByRole("article", { name: "You" }),
    ).toHaveTextContent("list the files");

    await user.click(codex);
    expect(codex).toHaveAttribute("aria-selected", "true");
    // The URL names the sibling so a reload comes back to the same agent.
    await waitFor(() =>
      expect(router.state.location.search).toMatchObject({ task: "sess-2" }),
    );
    await waitFor(() =>
      expect(screen.getByRole("article", { name: "You" })).toHaveTextContent(
        "run the tests",
      ),
    );
  });

  it("falls back to the main agent when ?task names a session that is gone", async () => {
    // A link outlives the agent it points at. Landing on one must not leave the
    // page showing nothing, and must not leave the URL still claiming it.
    const client = makeClient();
    client.listCodeWorkspaceSessions.mockResolvedValue([SESSION]);
    const { router } = await mountWorkspace(
      client,
      "/code/w/ws-1?task=sess-never-existed",
    );

    const main = await screen.findByRole("tab", { name: "Main agent" });
    expect(main).toHaveAttribute("aria-selected", "true");
    expect(
      await screen.findByRole("article", { name: "You" }),
    ).toHaveTextContent("list the files");
    await waitFor(() =>
      expect(router.state.location.search).not.toHaveProperty("task"),
    );
  });

  it("falls back to the main agent when ?task names an ended session", async () => {
    // Ended is the common case: the reader stopped the agent, then reloaded.
    // The session is still listed, so existence alone is not enough to select.
    const client = makeClient();
    const ended: CodeSessionSnapshot = {
      ...SESSION,
      id: "sess-2",
      harness_kind: "codex",
      lifecycle: "ended",
      created_at: "2026-08-15T01:00:00.000Z",
    };
    client.listCodeWorkspaceSessions.mockResolvedValue([ended, SESSION]);
    const { router } = await mountWorkspace(client, "/code/w/ws-1?task=sess-2");

    const main = await screen.findByRole("tab", { name: "Main agent" });
    expect(main).toHaveAttribute("aria-selected", "true");
    // An ended agent keeps no tab, so there is nothing to switch back to.
    expect(
      screen.queryByRole("tab", { name: "Codex CLI" }),
    ).not.toBeInTheDocument();
    await waitFor(() =>
      expect(router.state.location.search).not.toHaveProperty("task"),
    );
  });

  it("opens a draft tab for a second agent and closes it again", async () => {
    const client = makeClient();
    client.listCodeWorkspaceSessions.mockResolvedValue([SESSION]);
    const user = userEvent.setup();
    await mountWorkspace(client);

    await screen.findByRole("tab", { name: "Main agent" });
    await user.click(screen.getByRole("button", { name: "New tab" }));
    await user.click(
      await screen.findByRole("menuitem", { name: "New agent" }),
    );

    const draft = await screen.findByRole("tab", { name: "New agent" });
    expect(draft).toHaveAttribute("aria-selected", "true");
    // Nothing is running behind a draft, so the picker takes the panel.
    expect(
      await screen.findByRole("button", { name: "Send message" }),
    ).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Close New agent" }));
    expect(
      screen.queryByRole("tab", { name: "New agent" }),
    ).not.toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "Main agent" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
  });

  it("starts on the main-agent tab and opens a file from the visible new-tab control", async () => {
    const client = makeClient();
    client.listCodeWorkspaceTree.mockResolvedValue({
      paths: ["README.md", "src/lib.rs"],
      truncated: false,
    });
    const user = userEvent.setup();
    await mountWorkspace(client);

    const mainAgent = await screen.findByRole("tab", { name: "Main agent" });
    expect(mainAgent).toHaveAttribute("aria-selected", "true");
    const mainPanel = document.getElementById(
      mainAgent.getAttribute("aria-controls") ?? "",
    );
    expect(mainPanel).toHaveAttribute("role", "tabpanel");
    expect(mainPanel).toHaveAttribute("aria-labelledby", mainAgent.id);

    await user.click(screen.getByRole("button", { name: "New tab" }));
    await user.click(
      await screen.findByRole("menuitem", { name: "Open file…" }),
    );
    const picker = await screen.findByRole("combobox", {
      name: "Search files by name",
    });
    expect(picker).toHaveFocus();
    await user.click(await screen.findByRole("option", { name: "src/lib.rs" }));

    expect(await screen.findByRole("tab", { name: "lib.rs" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(screen.getByTestId("file-viewer")).toHaveTextContent("src/lib.rs");
  });

  it("attaches the watch task's transcript via ?task and offers its controls", async () => {
    const client = makeClient();
    const watchSession = {
      ...SESSION,
      id: "sess-watch",
      kind: "watch" as const,
      permission_mode: "auto" as const,
      fast_mode: false,
      lifecycle: "idle" as const,
    };
    client.listCodeWorkspaceSessions.mockResolvedValue([SESSION, watchSession]);
    client.getCodeWorkspacePr.mockResolvedValue({
      dirty: false,
      unpushed: false,
      ahead: 0,
      has_upstream: true,
      suggested_commit_message: "",
      pr: PR,
      gh_found: true,
      gh_authenticated: true,
      remediation: "",
      watch: {
        id: "watch-1",
        workspace_id: "ws-1",
        session_id: "sess-watch",
        pr_number: PR.number,
        state: "watching" as const,
        cycles: 1,
        created_at: "2026-08-20T09:00:00.000Z",
        updated_at: "2026-08-20T09:05:00.000Z",
      },
    });
    const user = userEvent.setup();
    const { router } = await mountWorkspace(
      client,
      "/code/w/ws-1?task=sess-watch&subagent=task-ignored",
    );

    const bar = await screen.findByTestId("watch-task-bar");
    expect(bar).toHaveTextContent(`Watching PR #${PR.number}`);
    expect(
      screen.queryByTestId("subagent-context-bar"),
    ).not.toBeInTheDocument();
    expect(router.state.location.search).not.toHaveProperty("subagent");
    // The watch bar replaces the composer: the sweep drives this session.
    expect(
      screen.queryByRole("textbox", { name: "Message" }),
    ).not.toBeInTheDocument();

    await user.click(
      within(bar).getByRole("button", { name: "Back to main task" }),
    );
    await waitFor(() =>
      expect(router.state.location.search).not.toHaveProperty(
        "task",
        "sess-watch",
      ),
    );
  });

  it("filters a subagent in the mounted parent session and preserves pane state on return", async () => {
    const callId = "toolu-task-audit";
    const client = makeClient();
    client.listCodeWorkspaceSessions.mockResolvedValue([SESSION]);
    client.openCodeEvents.mockImplementation((_sessionId, _after, onFrame) =>
      codeEventSocket(onFrame, [
        {
          seq: 1,
          replayed: true,
          event: {
            type: "assistant_message",
            text: "I delegated the parser audit.",
          },
        },
        {
          seq: 2,
          replayed: true,
          event: {
            type: "tool_started",
            call_id: callId,
            name: "Task",
            detail: { kind: "other", summary: "Audit the parser" },
          },
        },
        {
          seq: 3,
          replayed: true,
          event: {
            type: "tool_started",
            call_id: "child-read",
            name: "Read",
            detail: { kind: "file_read", path: "src/parser.rs" },
            parent_call_id: callId,
          },
        },
        {
          seq: 4,
          replayed: true,
          event: {
            type: "tool_completed",
            call_id: "child-read",
            outcome: "succeeded",
            preview: "parser source",
            parent_call_id: callId,
          },
        },
        {
          seq: 5,
          replayed: true,
          event: {
            type: "assistant_message",
            text: "The parser contract is sound.",
            parent_call_id: callId,
          },
        },
        {
          seq: 6,
          replayed: true,
          event: {
            type: "tool_completed",
            call_id: callId,
            outcome: "succeeded",
            preview: "Audit complete",
          },
        },
      ]),
    );
    useCodeUpdatesStore.getState().apply({
      type: "digest",
      digest: {
        workspace: WORKSPACE.id,
        session: SESSION.id,
        kind: "interactive",
        lifecycle: "idle",
        attention: SESSION.attention,
        title: WORKSPACE.title,
        turn_count: 1,
        subagents: [
          { call_id: callId, name: "Audit the parser", status: "done" },
        ],
      },
    });
    const user = userEvent.setup();
    const { router } = await mountWorkspace(
      client,
      `/code/w/ws-1?subagent=${callId}`,
    );

    await waitFor(() => {
      expect(screen.getByTestId("subagent-context-bar")).toHaveTextContent(
        "Audit the parser",
      );
      expect(screen.getByTestId("subagent-context-bar")).toHaveTextContent(
        "Completed",
      );
    });
    const context = screen.getByTestId("subagent-context-bar");
    expect(context).toHaveTextContent("Read-only subagent view");
    expect(
      await screen.findByText("The parser contract is sound."),
    ).toBeInTheDocument();
    expect(
      screen.queryByText("I delegated the parser audit."),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("textbox", { name: "Message" }),
    ).not.toBeInTheDocument();
    expect(client.openCodeEvents).toHaveBeenCalledTimes(1);
    expect(client.openCodeEvents).toHaveBeenCalledWith(
      SESSION.id,
      0,
      expect.any(Function),
    );

    await user.click(screen.getByRole("button", { name: "Terminal" }));
    await waitFor(() =>
      expect(router.state.location.search).toMatchObject({
        tabs: "terminal.term-1",
        subagent: callId,
      }),
    );

    // The terminal is a tab, so opening it covers the conversation. What the
    // reader was reading has to survive coming back to it.
    await user.click(screen.getByRole("tab", { name: "Main agent" }));
    await user.click(
      within(context).getByRole("button", { name: "Back to main agent" }),
    );
    await waitFor(() => {
      expect(router.state.location.search).not.toHaveProperty("subagent");
      expect(router.state.location.search).toMatchObject({
        tabs: "terminal.term-1",
      });
    });
    expect(client.openCodeEvents).toHaveBeenCalledTimes(1);
    expect(
      await screen.findByText("I delegated the parser audit."),
    ).toBeInTheDocument();
    expect(
      screen.queryByText("The parser contract is sound."),
    ).not.toBeInTheDocument();
    expect(
      screen.getByRole("textbox", { name: "Message" }),
    ).toBeInTheDocument();
  });

  it("recovers a stale digest row from the spanning Task in transcript history", async () => {
    const callId = "toolu-task-recovered";
    const client = makeClient();
    client.listCodeWorkspaceSessions.mockResolvedValue([SESSION]);
    client.openCodeEvents.mockImplementation((_sessionId, _after, onFrame) =>
      codeEventSocket(onFrame, [
        {
          seq: 1,
          replayed: true,
          event: {
            type: "tool_started",
            call_id: callId,
            name: "Task",
            detail: { kind: "other", summary: "Recover the interrupted audit" },
          },
        },
        {
          seq: 2,
          replayed: true,
          event: {
            type: "tool_completed",
            call_id: callId,
            outcome: "failed",
            preview: "Parent session recovered",
          },
        },
      ]),
    );

    await mountWorkspace(client, `/code/w/ws-1?subagent=${callId}`);

    await waitFor(() => {
      expect(screen.getByTestId("subagent-context-bar")).toHaveTextContent(
        "Recover the interrupted audit",
      );
      expect(screen.getByTestId("subagent-context-bar")).toHaveTextContent(
        "Failed",
      );
    });
    expect(
      await screen.findByText("No captured subagent output"),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("textbox", { name: "Message" }),
    ).not.toBeInTheDocument();
  });

  it("keeps an invalid subagent link recoverable without swapping sessions", async () => {
    const client = makeClient();
    client.listCodeWorkspaceSessions.mockResolvedValue([SESSION]);

    await mountWorkspace(client, "/code/w/ws-1?subagent=missing-task");

    const context = await screen.findByTestId("subagent-context-bar");
    expect(context).toHaveTextContent("Subagent unavailable");
    expect(context).toHaveTextContent("Unavailable");
    expect(
      await screen.findByText(
        "This link no longer matches a captured Task in the parent session.",
      ),
    ).toBeInTheDocument();
    expect(client.openCodeEvents).toHaveBeenCalledTimes(1);
    expect(client.openCodeEvents.mock.calls[0]?.[0]).toBe(SESSION.id);
  });

  it("opens source control and PR details as center tabs from the + menu", async () => {
    const client = makeClient();
    client.getCodeWorkspace.mockResolvedValue({ ...WORKSPACE, pr: PR });
    const user = userEvent.setup();
    await mountWorkspace(client);

    await user.click(screen.getByRole("button", { name: "New tab" }));
    await user.click(
      await screen.findByRole("menuitem", { name: "Source control" }),
    );
    // The inspector also names a Source control tab; the center's tab is the
    // one controlling the primary editor panel.
    const centerTab = (name: string) =>
      screen
        .getAllByRole("tab", { name })
        .find(
          (tab) =>
            tab.getAttribute("aria-controls") ===
            "code-center-panel-editor-primary",
        );
    await waitFor(() =>
      expect(centerTab("Source control")).toHaveAttribute(
        "aria-selected",
        "true",
      ),
    );
    expect(
      await screen.findByTestId("source-control-panel"),
    ).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "New tab" }));
    await user.click(
      await screen.findByRole("menuitem", { name: "Pull request" }),
    );
    await waitFor(() =>
      expect(centerTab("Pull request")).toHaveAttribute(
        "aria-selected",
        "true",
      ),
    );
    expect(await screen.findByTestId("pr-details-panel")).toBeInTheDocument();
  });

  it("opens, restores, and retitles a browser as a center editor tab", async () => {
    const client = makeClient();
    vi.spyOn(globalThis.crypto, "randomUUID").mockReturnValue(
      "browser-1" as `${string}-${string}-${string}-${string}-${string}`,
    );
    const user = userEvent.setup();
    const { router } = await mountWorkspace(client);

    await user.click(screen.getByRole("button", { name: "New tab" }));
    await user.click(
      await screen.findByRole("menuitem", { name: "New browser" }),
    );

    expect(await screen.findByRole("tab", { name: "Browser" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(
      await screen.findByTestId("browser-panel-browser-1"),
    ).toBeInTheDocument();
    expect(readStoredBrowserSession("browser-1")?.workspaceId).toBe("ws-1");
    await waitFor(() =>
      expect(router.state.location.search).toMatchObject({
        tabs: "browser.browser-1",
      }),
    );

    await user.click(screen.getByTestId("browser-panel-browser-1"));
    expect(
      screen.getByRole("tab", { name: "Tidebreak docs" }),
    ).toBeInTheDocument();
  });

  it("restores a persisted browser title without polling storage", async () => {
    const client = makeClient();
    const session = seedBrowserSession({
      browserId: "browser-restored",
      workspaceId: "ws-1",
      initialUrl: "https://docs.tidebreak.dev",
    });
    writeStoredBrowserSession({ ...session, title: "Tidebreak handbook" });

    await mountWorkspace(client, "/code/w/ws-1?tabs=browser.browser-restored");

    expect(
      await screen.findByRole("tab", { name: "Tidebreak handbook" }),
    ).toHaveAttribute("aria-selected", "true");
  });

  it("hides the native browser behind dialogs and portaled menus", async () => {
    const client = makeClient();
    seedBrowserSession({
      browserId: "browser-1",
      workspaceId: "ws-1",
    });
    const user = userEvent.setup();
    await mountWorkspace(client, "/code/w/ws-1?tabs=browser.browser-1");

    const panel = await screen.findByTestId("browser-panel-browser-1");
    expect(panel).toHaveAttribute("data-obscured", "false");

    await user.click(screen.getByRole("button", { name: "New tab" }));
    await waitFor(() => expect(panel).toHaveAttribute("data-obscured", "true"));
    await user.keyboard("{Escape}");
    await waitFor(() =>
      expect(panel).toHaveAttribute("data-obscured", "false"),
    );

    fireEvent.contextMenu(screen.getByRole("tab", { name: "Browser" }));
    await screen.findByRole("menu");
    await waitFor(() => expect(panel).toHaveAttribute("data-obscured", "true"));
  });

  it("closes a native browser only when its editor tab is removed", async () => {
    const client = makeClient();
    seedBrowserSession({ browserId: "browser-1", workspaceId: "ws-1" });
    const user = userEvent.setup();
    await mountWorkspace(
      client,
      "/code/w/ws-1?tabs=browser.browser-1&split=file.README.md",
    );

    await user.click(
      screen.getByRole("button", { name: "Move split tabs to main group" }),
    );
    expect(browserMocks.close).not.toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: "Close Browser" }));
    await waitFor(() =>
      expect(browserMocks.close).toHaveBeenCalledExactlyOnceWith(
        "ws-1",
        "browser-1",
      ),
    );
  });

  it("closes surviving native browser sessions when the workspace tears down", async () => {
    const client = makeClient();
    seedBrowserSession({ browserId: "browser-1", workspaceId: "ws-1" });
    const mounted = await mountWorkspace(
      client,
      "/code/w/ws-1?tabs=browser.browser-1",
    );
    await screen.findByTestId("browser-panel-browser-1");

    mounted.unmount();

    expect(browserMocks.close).toHaveBeenCalledExactlyOnceWith(
      "ws-1",
      "browser-1",
    );
  });

  it("moves file tabs into a reloadable split group and back", async () => {
    const client = makeClient();
    const user = userEvent.setup();
    const { router } = await mountWorkspace(
      client,
      "/code/w/ws-1?tabs=file.src%252Flib.rs,file.src%252Fmain.rs&active=file.src%252Fmain.rs",
    );

    const mainTab = await screen.findByRole("tab", { name: "main.rs" });
    fireEvent.contextMenu(mainTab);
    await user.click(
      await screen.findByRole("menuitem", { name: "Move to split right" }),
    );

    const splitStrip = await screen.findByRole("tablist", {
      name: "Workspace split",
    });
    expect(
      within(splitStrip).getByRole("tab", { name: "main.rs" }),
    ).toHaveAttribute("aria-selected", "true");
    await waitFor(() =>
      expect(router.state.location.search).toMatchObject({
        tabs: "file.src%2Flib.rs",
        split: "file.src%2Fmain.rs",
        splitFocused: "1",
      }),
    );

    await user.click(
      within(splitStrip).getByRole("button", {
        name: "Move split tabs to main group",
      }),
    );
    expect(
      screen.queryByRole("tablist", { name: "Workspace split" }),
    ).not.toBeInTheDocument();

    // The split zone belongs to a drag, so it is not on screen without one.
    expect(screen.queryByTestId("split-drop-zone")).not.toBeInTheDocument();
    await waitFor(() =>
      expect(router.state.location.search).toMatchObject({
        tabs: "file.src%2Flib.rs,file.src%2Fmain.rs",
      }),
    );
  });

  it("reorders a tab from its context menu, without a pointer drag", async () => {
    const client = makeClient();
    const user = userEvent.setup();
    const { router } = await mountWorkspace(
      client,
      "/code/w/ws-1?tabs=file.src%252Flib.rs,file.src%252Fmain.rs&active=file.src%252Fmain.rs",
    );

    // Dragging reorders too, but a pointer is the only way to reach it. This
    // menu is the same move for anyone on a keyboard, which is why the tab
    // wrapper stays out of the tab order rather than becoming a second stop.
    const libTab = await screen.findByRole("tab", { name: "lib.rs" });
    fireEvent.contextMenu(libTab);
    await user.click(
      await screen.findByRole("menuitem", { name: "Move right" }),
    );

    await waitFor(() =>
      expect(router.state.location.search).toMatchObject({
        tabs: "file.src%2Fmain.rs,file.src%2Flib.rs",
      }),
    );
  });

  // The reported bug was that a drag never started: the strip was the only
  // HTML5 drag source in the app, and WebKit would not begin an ancestor's
  // drag from the inner `<button role="tab">` the reader actually presses.
  // These two cover that gesture from the pointer down. Where it lands is a
  // separate question, and the pure rules in `editorDrag` answer it: dnd-kit
  // resolves a target from element rects, and jsdom reports every rect as
  // zero, so asserting a drop here would prove nothing.
  it("starts a tab drag from a pointer press on the tab itself", async () => {
    const client = makeClient();
    await mountWorkspace(
      client,
      "/code/w/ws-1?tabs=file.src%252Flib.rs,file.src%252Fmain.rs&active=file.src%252Fmain.rs",
    );

    const mainTab = await screen.findByRole("tab", { name: "main.rs" });
    expect(screen.queryByTestId("split-drop-zone")).not.toBeInTheDocument();

    dragBy(mainTab, 40);

    // The zone renders only while a drag is live, so its presence is the drag.
    expect(await screen.findByTestId("split-drop-zone")).toBeInTheDocument();
  });

  it("does not start a tab drag from a press on the close control", async () => {
    const client = makeClient();
    await mountWorkspace(
      client,
      "/code/w/ws-1?tabs=file.src%252Flib.rs,file.src%252Fmain.rs&active=file.src%252Fmain.rs",
    );

    await screen.findByRole("tab", { name: "main.rs" });
    const close = screen.getByRole("button", { name: "Close main.rs" });

    dragBy(close, 40);

    // A press that drifts a few pixels should still close the tab rather than
    // carry it somewhere, which is what the sensor's opt-out marker buys.
    expect(screen.queryByTestId("split-drop-zone")).not.toBeInTheDocument();
  });

  it("keeps the jump-to-latest pill out of the tab order until it is on screen", async () => {
    const client = makeClient();
    client.listCodeWorkspaceSessions.mockResolvedValue([SESSION]);
    await mountWorkspace(client);

    await screen.findByRole("button", { name: "Review this turn's changes" });
    // The pill is the keyboard path back to the tail. It is a real button, so
    // it must leave the tab order rather than sit there invisibly focusable.
    const pill = screen.getByLabelText("Scroll to latest");
    expect(pill.tagName).toBe("BUTTON");
    expect(pill).toHaveAttribute("aria-hidden", "true");
    expect(pill).toHaveAttribute("tabindex", "-1");
  });
});
