// @vitest-environment jsdom
import {
  act,
  cleanup,
  fireEvent,
  screen,
  waitFor,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { AppContextProvider, type AppContextValue } from "@/AppContext";
import { renderWithRouter } from "@/test/router";
import type {
  CodeRepoSnapshot,
  CodeSessionSnapshot,
  CodeWorkspaceSnapshot,
  HarnessDoctorEntry,
  HarnessKind,
} from "../api/types";
import {
  OPTIMISTIC_WORKSPACE_ID_PREFIX,
  useCodeCatalogStore,
} from "./CodeCatalogStore";
import { EMPTY_NEW_WORKSPACE_DRAFT, useCodeUiStore } from "./CodeUiStore";
import { NewWorkspaceDialog } from "./NewWorkspaceDialog";
import type { ReasoningEffort } from "../api/types";
import type { CodeTurnSubmission, ParsedHarnessModel } from "./parsers";

const toastError = vi.hoisted(() => vi.fn());
const toastSuccess = vi.hoisted(() => vi.fn());
vi.mock("sonner", () => ({
  toast: {
    error: toastError,
    success: toastSuccess,
  },
}));

afterEach(() => {
  cleanup();
  useCodeCatalogStore.getState().reset();
  useCodeUiStore.setState({
    lastCreate: null,
    pendingComposerPrompt: null,
    newWorkspaceDraft: EMPTY_NEW_WORKSPACE_DRAFT,
  });
  toastError.mockReset();
  toastSuccess.mockReset();
});

const CAPS = {
  resume: "supported",
  streaming_deltas: "supported",
  mid_turn_steering: "unsupported",
  plan_mode: "supported",
  auto_mode: "supported",
  allow_mode: "supported",
  reasoning_levels: "unknown",
  native_file_change_events: "unsupported",
  native_interrupt: "supported",
  structured_approvals: "supported",
  image_input: "unknown",
  slash_commands: "unknown",
} as const;

function harness(kind: HarnessKind): HarnessDoctorEntry {
  return {
    kind,
    found: true,
    installable: true,
    authenticated: true,
    tier: "reference",
    caps: { ...CAPS },
    commands: [],
    auth_mode: "local_sign_in",
    remediation: "",
    stderr: "",
    unrecognized_event_count: 0,
    relaunch_composes_permission_mode: kind !== "opencode",
  } as HarnessDoctorEntry;
}

function repo(id: string, name: string): CodeRepoSnapshot {
  return {
    id,
    root_path: `/tmp/${name}`,
    display_name: name,
    default_base_ref: "main",
    branch_prefix: "tidebreak/",
    quick_actions: [],
    created_at: "2026-08-01T00:00:00.000Z",
  } as CodeRepoSnapshot;
}

function workspace(id: string, repoId: string, createdAt: string) {
  return {
    id,
    repo_id: repoId,
    title: id,
    worktree_path: `/tmp/wt/${id}`,
    branch_name: `tidebreak/${id}`,
    base_ref: "main",
    status: "active",
    created_at: createdAt,
  } as CodeWorkspaceSnapshot;
}

function session(workspaceId: string, kind: HarnessKind, createdAt: string) {
  return {
    id: `sess-${workspaceId}`,
    workspace_id: workspaceId,
    harness_kind: kind,
    permission_mode: "ask",
    lifecycle: "idle",
    attention: { state: { type: "working" }, source: "engine" },
    unrecognized_event_count: 0,
    created_at: createdAt,
  } as unknown as CodeSessionSnapshot;
}

function claudeModels() {
  return vi.fn(async () => ({
    kind: "claude_code" as const,
    models: [
      {
        id: "sonnet",
        label: "Sonnet",
        default: true,
        reasoning_efforts: [],
        fast_mode: false,
      },
    ],
    reasoning_efforts: [],
    fast_mode: false,
  }));
}

function app(client: Partial<AppContextValue["client"]>): AppContextValue {
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
  } as AppContextValue;
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((done, fail) => {
    resolve = done;
    reject = fail;
  });
  return { promise, resolve, reject };
}

describe("NewWorkspaceDialog", () => {
  it("gives the prompt more room and lets controls wrap", async () => {
    const repos = [repo("repo-new", "tidebreak")];
    useCodeCatalogStore.setState({
      repos,
      doctor: {
        harnesses: [harness("claude_code")],
        notices: [],
      } as never,
    });
    await renderWithRouter(
      <AppContextProvider
        value={app({ listCodeHarnessModels: claudeModels() })}
      >
        <NewWorkspaceDialog open onOpenChange={vi.fn()} repos={repos} />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );

    expect(screen.getByRole("dialog")).toHaveClass(
      "max-w-4xl",
      "max-h-[calc(100dvh-1rem)]",
    );
    expect(screen.getByRole("textbox", { name: "First message" })).toHaveClass(
      "min-h-48",
      "sm:min-h-52",
    );
    expect(screen.getByTestId("new-workspace-controls")).toHaveClass(
      "flex-wrap",
    );
    // A short window scrolls the form rather than clipping the Create row.
    expect(screen.getByRole("dialog").querySelector("form")).toHaveClass(
      "min-h-0",
      "overflow-y-auto",
    );
  });

  it("closes and lists the workspace before creation finishes", async () => {
    const repos = [repo("repo-new", "tidebreak")];
    useCodeCatalogStore.setState({
      repos,
      doctor: {
        harnesses: [harness("claude_code")],
        notices: [],
      } as never,
    });
    const creation = deferred<CodeWorkspaceSnapshot>();
    const created = workspace(
      "ws-created",
      "repo-new",
      "2026-08-24T12:00:00.000Z",
    );
    const onOpenChange = vi.fn();
    const { router } = await renderWithRouter(
      <AppContextProvider
        value={app({
          createCodeWorkspace: vi.fn(() => creation.promise),
          createCodeSession: vi.fn(async () =>
            session("ws-created", "claude_code", "2026-08-24T12:00:00.000Z"),
          ),
          listCodeHarnessModels: claudeModels(),
        })}
      >
        <NewWorkspaceDialog open onOpenChange={onOpenChange} repos={repos} />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );

    fireEvent.keyDown(screen.getByRole("dialog"), {
      key: "Enter",
      metaKey: true,
    });

    expect(onOpenChange).toHaveBeenCalledWith(false);
    expect(router.state.location.pathname).toBe("/code");
    expect(useCodeCatalogStore.getState().workspaces).toEqual([
      expect.objectContaining({
        id: expect.stringMatching(`^${OPTIMISTIC_WORKSPACE_ID_PREFIX}`),
        status: "creating",
        title: "New workspace",
      }),
    ]);

    creation.resolve(created);
    await waitFor(() =>
      expect(router.state.location.pathname).toBe("/code/w/ws-created"),
    );
    expect(useCodeCatalogStore.getState().workspaces).toEqual([created]);
  });

  it("opens the workspace as soon as it exists, before the session starts", async () => {
    const repos = [repo("repo-new", "tidebreak")];
    useCodeCatalogStore.setState({
      repos,
      doctor: {
        harnesses: [harness("claude_code")],
        notices: [],
      } as never,
    });
    const created = workspace(
      "ws-early",
      "repo-new",
      "2026-08-24T12:00:00.000Z",
    );
    const sessionStart = deferred<CodeSessionSnapshot>();
    const { router } = await renderWithRouter(
      <AppContextProvider
        value={app({
          createCodeWorkspace: vi.fn(async () => created),
          createCodeSession: vi.fn(() => sessionStart.promise),
          listCodeHarnessModels: claudeModels(),
        })}
      >
        <NewWorkspaceDialog open onOpenChange={vi.fn()} repos={repos} />
      </AppContextProvider>,
      { initialUrl: "/code/w/ws-old" },
    );

    fireEvent.keyDown(screen.getByRole("dialog"), {
      key: "Enter",
      metaKey: true,
    });

    await waitFor(() =>
      expect(router.state.location.pathname).toBe("/code/w/ws-early"),
    );
    sessionStart.resolve(
      session("ws-early", "claude_code", "2026-08-24T12:00:00.000Z"),
    );
  });

  it("removes a failed create and retries the captured request", async () => {
    const repos = [repo("repo-new", "tidebreak")];
    useCodeCatalogStore.setState({
      repos,
      doctor: {
        harnesses: [harness("claude_code")],
        notices: [],
      } as never,
    });
    const created = workspace(
      "ws-retried",
      "repo-new",
      "2026-08-24T12:00:00.000Z",
    );
    const createCodeWorkspace = vi
      .fn()
      .mockRejectedValueOnce(new Error("base ref moved"))
      .mockResolvedValueOnce(created);
    await renderWithRouter(
      <AppContextProvider
        value={app({
          createCodeWorkspace,
          createCodeSession: vi.fn(async () =>
            session("ws-retried", "claude_code", "2026-08-24T12:00:00.000Z"),
          ),
          listCodeHarnessModels: claudeModels(),
        })}
      >
        <NewWorkspaceDialog open onOpenChange={vi.fn()} repos={repos} />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );

    fireEvent.change(screen.getByRole("textbox", { name: "First message" }), {
      target: { value: "keep this request" },
    });
    fireEvent.keyDown(screen.getByRole("dialog"), {
      key: "Enter",
      metaKey: true,
    });

    await waitFor(() => expect(toastError).toHaveBeenCalledOnce());
    expect(useCodeCatalogStore.getState().workspaces).toEqual([]);
    const toastOptions = toastError.mock.calls[0]?.[1] as {
      action: { onClick: () => void };
    };
    toastOptions.action.onClick();

    await waitFor(() => expect(createCodeWorkspace).toHaveBeenCalledTimes(2));
    expect(createCodeWorkspace).toHaveBeenLastCalledWith({
      repo_id: "repo-new",
      title: undefined,
      base_ref: "main",
    });
    await waitFor(() =>
      expect(useCodeCatalogStore.getState().workspaces).toEqual([created]),
    );
  });

  it("keeps remembered models keyed by harness", () => {
    useCodeUiStore.getState().rememberCreate({
      repoId: "repo-new",
      harness: "opencode",
      model: "model-gateway/deepseek-v4-pro",
    });
    useCodeUiStore.getState().rememberCreate({
      repoId: "repo-new",
      harness: "claude_code",
      model: "claude-opus-5",
    });

    expect(useCodeUiStore.getState().lastCreate).toEqual({
      repoId: "repo-new",
      harness: "claude_code",
      modelsByHarness: {
        opencode: "model-gateway/deepseek-v4-pro",
        claude_code: "claude-opus-5",
      },
      reasoningEffortByHarness: {},
      fastModeByHarness: {},
    });
  });

  it("says the mode is fixed once an opencode session starts", async () => {
    const repos = [repo("repo-new", "tidebreak")];
    useCodeCatalogStore.setState({
      repos,
      doctor: {
        harnesses: [harness("opencode")],
      } as never,
    });
    await renderWithRouter(
      <AppContextProvider
        value={app({
          listCodeHarnessModels: vi.fn(async () => ({
            kind: "opencode" as const,
            models: [],
            reasoning_efforts: [],
          })),
        })}
      >
        <NewWorkspaceDialog open onOpenChange={vi.fn()} repos={repos} />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );

    expect(
      screen.getByText("Mode is fixed once the session starts"),
    ).toBeInTheDocument();
  });

  // A report that has not landed knows nothing about any engine, so the
  // dialog falls back to a guess to render. Downloading on that guess fetches
  // hundreds of megabytes of whichever engine the fallback named, and takes
  // the dialog down on any client that cannot install at all.
  it("downloads nothing until the doctor names a missing engine", async () => {
    const repos = [repo("repo-new", "tidebreak")];
    useCodeCatalogStore.setState({
      repos,
      workspaces: [],
      sessionsByWorkspace: {},
      doctor: { harnesses: [] } as never,
    });
    const startHarnessInstall = vi.fn(async () => {
      throw new Error("the dialog must not ask for this");
    });
    await renderWithRouter(
      <AppContextProvider
        value={app({
          startHarnessInstall,
          listCodeHarnessModels: claudeModels(),
        })}
      >
        <NewWorkspaceDialog open onOpenChange={vi.fn()} repos={repos} />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );

    expect(
      screen.getByRole("textbox", { name: "First message" }),
    ).toBeInTheDocument();
    expect(startHarnessInstall).not.toHaveBeenCalled();
  });

  it("downloads the engine the doctor reports missing", async () => {
    const repos = [repo("repo-new", "tidebreak")];
    useCodeCatalogStore.setState({
      repos,
      workspaces: [],
      sessionsByWorkspace: {},
      doctor: {
        harnesses: [{ ...harness("claude_code"), found: false }],
      } as never,
    });
    const startHarnessInstall = vi.fn(async () => ({
      kind: "claude_code" as const,
      version: "2.1.234",
      phase: "installing",
      done: false,
    }));
    await renderWithRouter(
      <AppContextProvider
        value={app({
          startHarnessInstall,
          listCodeHarnessModels: claudeModels(),
        })}
      >
        <NewWorkspaceDialog open onOpenChange={vi.fn()} repos={repos} />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );

    await waitFor(() =>
      expect(startHarnessInstall).toHaveBeenCalledWith("claude_code"),
    );
    // Create waits for the pin rather than stalling minutes on it.
    expect(screen.getByRole("button", { name: /Create/ })).toBeDisabled();
  });

  it("opens on the last repo, harness, and model, and creates on Cmd+Enter", async () => {
    const repos = [repo("repo-old", "legacy"), repo("repo-new", "tidebreak")];
    useCodeCatalogStore.setState({
      repos,
      workspaces: [
        workspace("ws-1", "repo-old", "2026-08-10T00:00:00.000Z"),
        workspace("ws-2", "repo-new", "2026-08-17T00:00:00.000Z"),
      ],
      sessionsByWorkspace: {
        "ws-1": session("ws-1", "claude_code", "2026-08-10T00:00:00.000Z"),
        "ws-2": session("ws-2", "codex", "2026-08-17T00:00:00.000Z"),
      },
      doctor: {
        harnesses: [harness("claude_code"), harness("codex")],
        notices: [],
      } as never,
    });
    const createCodeWorkspace = vi.fn(async () =>
      workspace("ws-3", "repo-new", "2026-08-18T00:00:00.000Z"),
    );
    const createCodeSession = vi.fn(async () =>
      session("ws-3", "codex", "2026-08-18T00:00:00.000Z"),
    );
    await renderWithRouter(
      <AppContextProvider
        value={app({
          createCodeWorkspace,
          createCodeSession,
          listCodeHarnessModels: vi.fn(async () => ({
            kind: "codex" as const,
            models: [
              {
                id: "gpt-5.6-sol",
                label: "GPT 5.6 Sol",
                default: true,
                reasoning_efforts: [],
                fast_mode: false,
              },
            ],
            reasoning_efforts: [],
            fast_mode: false,
          })),
        })}
      >
        <NewWorkspaceDialog open onOpenChange={vi.fn()} repos={repos} />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );

    // Prompt-centric: the message is where focus lands, not a settings field.
    expect(
      screen.getByRole("textbox", { name: "First message" }),
    ).toHaveFocus();
    expect(screen.getByRole("button", { name: "Repo" })).toHaveTextContent(
      "tidebreak",
    );
    expect(
      screen.getByRole("button", { name: "Harness: Codex CLI" }),
    ).toBeEnabled();
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Model: GPT 5.6 Sol" }),
      ).toBeEnabled(),
    );
    // Allow is the default where the engine honors it.
    expect(
      screen.getByRole("button", { name: "Permissions: Allow all" }),
    ).toBeEnabled();
    expect(screen.queryByText(/runs without asking/)).toBeNull();

    fireEvent.keyDown(screen.getByRole("dialog"), {
      key: "Enter",
      metaKey: true,
    });
    await waitFor(() =>
      expect(createCodeWorkspace).toHaveBeenCalledWith({
        repo_id: "repo-new",
        title: undefined,
        base_ref: "main",
      }),
    );
    expect(createCodeSession).toHaveBeenCalledWith("ws-3", {
      harness: "codex",
      permission_mode: "allow",
      model: "gpt-5.6-sol",
    });
    await waitFor(() =>
      expect(useCodeUiStore.getState().lastCreate).toEqual({
        repoId: "repo-new",
        harness: "codex",
        modelsByHarness: { codex: "gpt-5.6-sol" },
        permissionMode: "allow",
        reasoningEffortByHarness: {},
        fastModeByHarness: { codex: false },
      }),
    );
    expect(useCodeUiStore.getState().pendingComposerPrompt).toBeNull();
  });

  it("creates on Enter in the message; Shift+Enter stays a newline", async () => {
    const repos = [repo("repo-new", "tidebreak")];
    useCodeCatalogStore.setState({
      repos,
      doctor: {
        harnesses: [harness("claude_code")],
        notices: [],
      } as never,
    });
    const createCodeWorkspace = vi.fn(async () =>
      workspace("ws-enter", "repo-new", "2026-08-20T00:00:00.000Z"),
    );
    await renderWithRouter(
      <AppContextProvider
        value={app({
          createCodeWorkspace,
          createCodeSession: vi.fn(async () =>
            session("ws-enter", "claude_code", "2026-08-20T00:00:00.000Z"),
          ),
          submitCodeTurn: vi.fn(
            async () => ({ kind: "turn" }) as unknown as CodeTurnSubmission,
          ),
          listCodeHarnessModels: claudeModels(),
        })}
      >
        <NewWorkspaceDialog open onOpenChange={vi.fn()} repos={repos} />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );

    const message = screen.getByRole("textbox", { name: "First message" });
    fireEvent.change(message, { target: { value: "ship the fix" } });
    fireEvent.keyDown(message, { key: "Enter", shiftKey: true });
    expect(createCodeWorkspace).not.toHaveBeenCalled();
    fireEvent.keyDown(message, { key: "Enter" });
    await waitFor(() => expect(createCodeWorkspace).toHaveBeenCalled());
  });

  it("keeps the first message after the dialog is dismissed", async () => {
    const repos = [repo("repo-new", "tidebreak")];
    useCodeCatalogStore.setState({
      repos,
      doctor: {
        harnesses: [harness("claude_code")],
        notices: [],
      } as never,
    });
    const value = app({ listCodeHarnessModels: claudeModels() });

    await renderWithRouter(
      <AppContextProvider value={value}>
        <NewWorkspaceDialog open onOpenChange={vi.fn()} repos={repos} />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );
    fireEvent.change(screen.getByRole("textbox", { name: "First message" }), {
      target: { value: "keep this task" },
    });
    cleanup();

    await renderWithRouter(
      <AppContextProvider value={value}>
        <NewWorkspaceDialog open onOpenChange={vi.fn()} repos={repos} />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );
    expect(screen.getByRole("textbox", { name: "First message" })).toHaveValue(
      "keep this task",
    );
  });

  it("sends the first message as the session's first turn", async () => {
    const repos = [repo("repo-new", "tidebreak")];
    useCodeCatalogStore.setState({
      repos,
      doctor: {
        harnesses: [harness("claude_code")],
        notices: [],
      } as never,
    });
    const createCodeWorkspace = vi.fn(async () =>
      workspace("ws-prompt", "repo-new", "2026-08-20T00:00:00.000Z"),
    );
    const createCodeSession = vi.fn(async () =>
      session("ws-prompt", "claude_code", "2026-08-20T00:00:00.000Z"),
    );
    const submitCodeTurn = vi.fn(
      async () => ({ kind: "turn" }) as unknown as CodeTurnSubmission,
    );
    const { router } = await renderWithRouter(
      <AppContextProvider
        value={app({
          createCodeWorkspace,
          createCodeSession,
          submitCodeTurn,
          listCodeHarnessModels: claudeModels(),
        })}
      >
        <NewWorkspaceDialog open onOpenChange={vi.fn()} repos={repos} />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );

    fireEvent.change(screen.getByRole("textbox", { name: "First message" }), {
      target: { value: "  list the files  " },
    });
    fireEvent.keyDown(screen.getByRole("dialog"), {
      key: "Enter",
      metaKey: true,
    });

    await waitFor(() =>
      expect(router.state.location.pathname).toBe("/code/w/ws-prompt"),
    );
    expect(submitCodeTurn).toHaveBeenCalledWith(
      "sess-ws-prompt",
      "list the files",
    );
    // Sent, not parked: nothing left for the workspace composer to take.
    expect(useCodeUiStore.getState().pendingComposerPrompt).toBeNull();
    expect(useCodeUiStore.getState().newWorkspaceDraft).toEqual(
      EMPTY_NEW_WORKSPACE_DRAFT,
    );
  });

  it("hands the message to the workspace composer when the turn cannot be sent", async () => {
    const repos = [repo("repo-new", "tidebreak")];
    useCodeCatalogStore.setState({
      repos,
      doctor: {
        harnesses: [harness("claude_code")],
        notices: [],
      } as never,
    });
    const submitCodeTurn = vi.fn(async () => {
      throw new Error("engine crashed on spawn");
    });
    const { router } = await renderWithRouter(
      <AppContextProvider
        value={app({
          createCodeWorkspace: vi.fn(async () =>
            workspace("ws-held", "repo-new", "2026-08-20T00:00:00.000Z"),
          ),
          createCodeSession: vi.fn(async () =>
            session("ws-held", "claude_code", "2026-08-20T00:00:00.000Z"),
          ),
          submitCodeTurn,
          listCodeHarnessModels: claudeModels(),
        })}
      >
        <NewWorkspaceDialog open onOpenChange={vi.fn()} repos={repos} />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );

    fireEvent.change(screen.getByRole("textbox", { name: "First message" }), {
      target: { value: "list the files" },
    });
    fireEvent.keyDown(screen.getByRole("dialog"), {
      key: "Enter",
      metaKey: true,
    });

    await waitFor(() =>
      expect(router.state.location.pathname).toBe("/code/w/ws-held"),
    );
    expect(useCodeUiStore.getState().pendingComposerPrompt).toEqual({
      scope: "ws-held",
      text: "list the files",
      submit: false,
    });
    expect(toastError).toHaveBeenCalledWith(
      "Session started, but the first message could not be sent. engine crashed on spawn",
    );
  });

  it("drops the previous harness model while the next catalog loads", async () => {
    const repos = [repo("repo-new", "tidebreak")];
    useCodeCatalogStore.setState({
      repos,
      doctor: {
        harnesses: [harness("claude_code"), harness("codex")],
        notices: [],
      } as never,
    });
    const codex = deferred<{
      kind: "codex";
      models: ParsedHarnessModel[];
      reasoning_efforts: ReasoningEffort[];
    }>();
    const listCodeHarnessModels = vi.fn((kind: HarnessKind) =>
      kind === "codex"
        ? codex.promise
        : Promise.resolve({
            kind: "claude_code" as const,
            models: [
              {
                id: "sonnet",
                label: "Sonnet",
                default: true,
                reasoning_efforts: [],
                fast_mode: false,
              },
            ],
            reasoning_efforts: [],
            fast_mode: false,
          }),
    );
    await renderWithRouter(
      <AppContextProvider value={app({ listCodeHarnessModels })}>
        <NewWorkspaceDialog open onOpenChange={vi.fn()} repos={repos} />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );

    const user = userEvent.setup();
    expect(
      await screen.findByRole("button", { name: "Model: Sonnet" }),
    ).toBeInTheDocument();
    await user.click(
      screen.getByRole("button", { name: "Harness: Claude Code" }),
    );
    await user.click(screen.getByRole("menuitem", { name: /Codex CLI/ }));

    expect(
      screen.queryByRole("button", { name: "Model: Sonnet" }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Loading models" }),
    ).toBeDisabled();

    await act(async () => {
      codex.resolve({
        kind: "codex",
        models: [
          {
            id: "gpt-5.6-luna",
            label: "GPT 5.6 Luna",
            default: true,
            reasoning_efforts: [],
            fast_mode: false,
          },
        ],
        reasoning_efforts: [],
      });
    });
    expect(
      await screen.findByRole("button", { name: "Model: GPT 5.6 Luna" }),
    ).toBeEnabled();
  });

  it("remembers a separate model for each harness", async () => {
    const repos = [repo("repo-new", "tidebreak")];
    useCodeCatalogStore.setState({
      repos,
      doctor: {
        harnesses: [harness("claude_code"), harness("opencode")],
        notices: [],
      } as never,
    });
    const listCodeHarnessModels = vi.fn(async (kind: HarnessKind) => ({
      kind,
      models:
        kind === "opencode"
          ? [
              {
                id: "model-gateway/kimi-k3",
                label: "Kimi K3",
                default: true,
                reasoning_efforts: [],
                fast_mode: false,
              },
              {
                id: "model-gateway/deepseek-v4-pro",
                label: "DeepSeek V4 Pro",
                default: false,
                reasoning_efforts: [],
                fast_mode: false,
              },
            ]
          : [
              {
                id: "claude-sonnet-5",
                label: "Claude Sonnet 5",
                default: true,
                reasoning_efforts: [],
                fast_mode: false,
              },
              {
                id: "claude-opus-5",
                label: "Claude Opus 5",
                default: false,
                reasoning_efforts: [],
                fast_mode: false,
              },
            ],
      reasoning_efforts: [],
    }));
    await renderWithRouter(
      <AppContextProvider value={app({ listCodeHarnessModels })}>
        <NewWorkspaceDialog open onOpenChange={vi.fn()} repos={repos} />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );

    const user = userEvent.setup();
    await user.click(
      await screen.findByRole("button", { name: "Model: Claude Sonnet 5" }),
    );
    await user.click(screen.getByRole("menuitem", { name: /Claude Opus 5/ }));

    await user.click(
      screen.getByRole("button", { name: "Harness: Claude Code" }),
    );
    await user.click(screen.getByRole("menuitem", { name: /opencode/ }));
    await user.click(
      await screen.findByRole("button", { name: "Model: Kimi K3" }),
    );
    await user.click(screen.getByRole("menuitem", { name: /DeepSeek V4 Pro/ }));

    await user.click(screen.getByRole("button", { name: "Harness: opencode" }));
    await user.click(screen.getByRole("menuitem", { name: /Claude Code/ }));
    expect(
      await screen.findByRole("button", { name: "Model: Claude Opus 5" }),
    ).toBeEnabled();

    await user.click(
      screen.getByRole("button", { name: "Harness: Claude Code" }),
    );
    await user.click(screen.getByRole("menuitem", { name: /opencode/ }));
    expect(
      await screen.findByRole("button", { name: "Model: DeepSeek V4 Pro" }),
    ).toBeEnabled();
  });

  it("keeps the repo picker enabled when opened from a repo", async () => {
    const repos = [repo("repo-old", "legacy"), repo("repo-new", "tidebreak")];
    useCodeCatalogStore.setState({
      repos,
      doctor: {
        harnesses: [harness("claude_code")],
        notices: [],
      } as never,
    });
    await renderWithRouter(
      <AppContextProvider
        value={app({
          listCodeHarnessModels: claudeModels(),
        })}
      >
        <NewWorkspaceDialog
          open
          onOpenChange={vi.fn()}
          repos={repos}
          defaultRepoId="repo-old"
        />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );

    const repoField = screen.getByRole("button", { name: "Repo" });
    expect(repoField).toHaveTextContent("legacy");
    expect(repoField).toBeEnabled();
  });

  it("opens a created workspace when its first session cannot start", async () => {
    const repos = [repo("repo-new", "tidebreak")];
    useCodeCatalogStore.setState({
      repos,
      doctor: {
        harnesses: [harness("codex")],
        notices: [],
      } as never,
    });
    const created = workspace(
      "ws-recover",
      "repo-new",
      "2026-08-19T00:00:00.000Z",
    );
    const onOpenChange = vi.fn();
    const createCodeSession = vi.fn(async () => {
      throw new Error("Codex sign-in expired");
    });
    const { router } = await renderWithRouter(
      <AppContextProvider
        value={app({
          createCodeWorkspace: vi.fn(async () => created),
          createCodeSession,
          listCodeHarnessModels: vi.fn(async () => ({
            kind: "codex" as const,
            models: [
              {
                id: "gpt-5.6-luna",
                label: "GPT 5.6 Luna",
                default: true,
                reasoning_efforts: [],
                fast_mode: false,
              },
            ],
            reasoning_efforts: [],
            fast_mode: false,
          })),
        })}
      >
        <NewWorkspaceDialog open onOpenChange={onOpenChange} repos={repos} />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );

    fireEvent.change(screen.getByRole("textbox", { name: "First message" }), {
      target: { value: "list the files" },
    });
    fireEvent.keyDown(screen.getByRole("dialog"), {
      key: "Enter",
      metaKey: true,
    });

    await waitFor(() =>
      expect(router.state.location.pathname).toBe("/code/w/ws-recover"),
    );
    expect(onOpenChange).toHaveBeenCalledWith(false);
    expect(useCodeCatalogStore.getState().workspaces).toContainEqual(created);
    expect(
      useCodeCatalogStore.getState().sessionsByWorkspace[created.id],
    ).toBeUndefined();
    // No session to send to, so the message waits in the workspace composer.
    expect(useCodeUiStore.getState().pendingComposerPrompt).toEqual({
      scope: "ws-recover",
      text: "list the files",
      submit: false,
    });
    expect(toastError).toHaveBeenCalledWith(
      "Workspace created, but the session could not start. Codex sign-in expired",
    );
  });

  it("keeps Enter in the name and base popovers out of create", async () => {
    const repos = [repo("repo-new", "tidebreak")];
    useCodeCatalogStore.setState({
      repos,
      doctor: {
        harnesses: [harness("claude_code")],
        notices: [],
      } as never,
    });
    const createCodeWorkspace = vi.fn(async () =>
      workspace("ws-typed", "repo-new", "2026-08-21T00:00:00.000Z"),
    );
    await renderWithRouter(
      <AppContextProvider
        value={app({
          createCodeWorkspace,
          createCodeSession: vi.fn(async () =>
            session("ws-typed", "claude_code", "2026-08-21T00:00:00.000Z"),
          ),
          listCodeHarnessModels: claudeModels(),
        })}
      >
        <NewWorkspaceDialog open onOpenChange={vi.fn()} repos={repos} />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );

    const user = userEvent.setup();
    // Enter in a popover field means "done typing", never "cut a worktree".
    await user.click(screen.getByRole("button", { name: "Workspace name" }));
    await user.type(
      screen.getByRole("textbox", { name: "Name" }),
      "rail polish{Enter}",
    );
    expect(
      screen.queryByRole("textbox", { name: "Name" }),
    ).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Base ref" }));
    await user.type(
      screen.getByRole("textbox", { name: "Base ref" }),
      "{Enter}",
    );
    expect(createCodeWorkspace).not.toHaveBeenCalled();

    fireEvent.keyDown(screen.getByRole("dialog"), {
      key: "Enter",
      metaKey: true,
    });
    await waitFor(() =>
      expect(createCodeWorkspace).toHaveBeenCalledWith({
        repo_id: "repo-new",
        title: "rail polish",
        base_ref: "main",
      }),
    );
  });

  it("stays open and clears the message when Create more is on", async () => {
    const repos = [repo("repo-new", "tidebreak")];
    useCodeCatalogStore.setState({
      repos,
      doctor: {
        harnesses: [harness("claude_code")],
        notices: [],
      } as never,
    });
    let creates = 0;
    const createCodeWorkspace = vi.fn(async () => {
      creates += 1;
      return workspace(
        `ws-more-${creates}`,
        "repo-new",
        "2026-08-21T00:00:00.000Z",
      );
    });
    const createCodeSession = vi.fn(async () =>
      session("ws-more", "claude_code", "2026-08-21T00:00:00.000Z"),
    );
    const submitCodeTurn = vi.fn(
      async () => ({ kind: "turn" }) as unknown as CodeTurnSubmission,
    );
    const onOpenChange = vi.fn();
    const { router } = await renderWithRouter(
      <AppContextProvider
        value={app({
          createCodeWorkspace,
          createCodeSession,
          submitCodeTurn,
          listCodeHarnessModels: claudeModels(),
        })}
      >
        <NewWorkspaceDialog open onOpenChange={onOpenChange} repos={repos} />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );

    const user = userEvent.setup();
    await user.click(screen.getByRole("switch", { name: "Create more" }));
    const message = screen.getByRole("textbox", { name: "First message" });
    fireEvent.change(message, { target: { value: "first task" } });
    fireEvent.keyDown(screen.getByRole("dialog"), {
      key: "Enter",
      metaKey: true,
    });
    await waitFor(() => expect(submitCodeTurn).toHaveBeenCalledTimes(1));

    // Still here, message cleared, settings kept — ready to fire the next one.
    expect(onOpenChange).not.toHaveBeenCalled();
    expect(router.state.location.pathname).toBe("/code");
    expect(message).toHaveValue("");
    expect(toastSuccess).toHaveBeenCalled();

    fireEvent.change(message, { target: { value: "second task" } });
    fireEvent.keyDown(screen.getByRole("dialog"), {
      key: "Enter",
      metaKey: true,
    });
    await waitFor(() => expect(createCodeWorkspace).toHaveBeenCalledTimes(2));
    expect(submitCodeTurn).toHaveBeenLastCalledWith(
      "sess-ws-more",
      "second task",
    );
  });

  it("toggles the pickers from their chords", async () => {
    const repos = [repo("repo-new", "tidebreak")];
    useCodeCatalogStore.setState({
      repos,
      doctor: {
        harnesses: [harness("claude_code")],
        notices: [],
      } as never,
    });
    await renderWithRouter(
      <AppContextProvider
        value={app({
          listCodeHarnessModels: claudeModels(),
        })}
      >
        <NewWorkspaceDialog open onOpenChange={vi.fn()} repos={repos} />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );
    await screen.findByRole("button", { name: "Model: Sonnet" });

    const dialog = screen.getByRole("dialog");
    // Cmd+N again: the repo menu, matching the shell chord that opened this.
    fireEvent.keyDown(dialog, { code: "KeyN", metaKey: true });
    expect(
      await screen.findByRole("menuitem", { name: /tidebreak/ }),
    ).toBeInTheDocument();
    fireEvent.keyDown(dialog, { code: "KeyN", metaKey: true });
    await waitFor(() =>
      expect(
        screen.queryByRole("menuitem", { name: /tidebreak/ }),
      ).not.toBeInTheDocument(),
    );

    // Alt+M: the model menu, search and all.
    fireEvent.keyDown(dialog, { code: "KeyM", altKey: true });
    expect(
      await screen.findByRole("searchbox", { name: "Search models" }),
    ).toBeInTheDocument();

    // Alt+B swaps straight over to the base popover.
    fireEvent.keyDown(dialog, { code: "KeyB", altKey: true });
    expect(
      await screen.findByRole("textbox", { name: "Base ref" }),
    ).toBeInTheDocument();
    await waitFor(() =>
      expect(
        screen.queryByRole("searchbox", { name: "Search models" }),
      ).not.toBeInTheDocument(),
    );
  });

  it("opens on the remembered permission mode when the engine honors it", async () => {
    const repos = [repo("repo-new", "tidebreak")];
    useCodeCatalogStore.setState({
      repos,
      doctor: {
        harnesses: [harness("claude_code")],
        notices: [],
      } as never,
    });
    useCodeUiStore.setState({
      lastCreate: { modelsByHarness: {}, permissionMode: "plan" },
    });
    await renderWithRouter(
      <AppContextProvider
        value={app({
          listCodeHarnessModels: claudeModels(),
        })}
      >
        <NewWorkspaceDialog open onOpenChange={vi.fn()} repos={repos} />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );

    // The engine's default would be Allow; the reader's last pick wins.
    expect(
      screen.getByRole("button", { name: "Permissions: Plan" }),
    ).toBeEnabled();
  });

  it("opens a mixed model catalog on every vendor at once", async () => {
    const repos = [repo("repo-new", "tidebreak")];
    useCodeCatalogStore.setState({
      repos,
      doctor: {
        harnesses: [harness("opencode")],
        notices: [],
      } as never,
    });
    await renderWithRouter(
      <AppContextProvider
        value={app({
          listCodeHarnessModels: vi.fn(async () => ({
            kind: "opencode" as HarnessKind,
            models: [
              {
                id: "gpt-5.6-sol",
                label: "GPT 5.6 Sol",
                default: true,
                reasoning_efforts: [],
                fast_mode: false,
              },
              {
                id: "claude-opus-5",
                label: "Claude Opus 5",
                default: false,
                reasoning_efforts: [],
                fast_mode: false,
              },
            ],
            reasoning_efforts: [],
            fast_mode: false,
          })),
        })}
      >
        <NewWorkspaceDialog open onOpenChange={vi.fn()} repos={repos} />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );

    const user = userEvent.setup();
    await user.click(
      await screen.findByRole("button", { name: "Model: GPT 5.6 Sol" }),
    );
    // Both vendors are one click away, not hidden behind the rail.
    expect(
      screen.getByRole("menuitem", { name: /Claude Opus 5/ }),
    ).toBeInTheDocument();
    await user.click(screen.getByRole("menuitem", { name: /Claude Opus 5/ }));
    expect(
      await screen.findByRole("button", { name: "Model: Claude Opus 5" }),
    ).toBeInTheDocument();

    // The rail still narrows to one vendor.
    await user.click(
      screen.getByRole("button", { name: "Model: Claude Opus 5" }),
    );
    await user.click(screen.getByRole("tab", { name: "OpenAI" }));
    expect(
      screen.queryByRole("menuitem", { name: /Claude Opus 5/ }),
    ).not.toBeInTheDocument();
  });

  it("lets the reader search the model list", async () => {
    const repos = [repo("repo-new", "tidebreak")];
    useCodeCatalogStore.setState({
      repos,
      doctor: {
        harnesses: [harness("opencode")],
        notices: [],
      } as never,
    });
    await renderWithRouter(
      <AppContextProvider
        value={app({
          listCodeHarnessModels: vi.fn(async () => ({
            kind: "opencode" as HarnessKind,
            models: [
              {
                id: "gpt-5.6-sol",
                label: "GPT 5.6 Sol",
                default: true,
                reasoning_efforts: [],
                fast_mode: false,
              },
              {
                id: "claude-opus-5",
                label: "Claude Opus 5",
                default: false,
                reasoning_efforts: [],
                fast_mode: false,
              },
              {
                id: "grok-4.5",
                label: "Grok 4.5",
                default: false,
                reasoning_efforts: [],
                fast_mode: false,
              },
            ],
            reasoning_efforts: [],
            fast_mode: false,
          })),
        })}
      >
        <NewWorkspaceDialog open onOpenChange={vi.fn()} repos={repos} />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );

    const user = userEvent.setup();
    const trigger = await screen.findByRole("button", {
      name: "Model: GPT 5.6 Sol",
    });
    await user.click(trigger);
    const search = await screen.findByRole("searchbox", {
      name: "Search models",
    });
    await user.type(search, "opus");
    expect(
      screen.getByRole("menuitem", { name: /Claude Opus 5/ }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("menuitem", { name: /GPT 5.6 Sol/ }),
    ).not.toBeInTheDocument();
  });

  it("offers reasoning effort and fast mode only when the engine and model honor them", async () => {
    const repos = [repo("repo-new", "tidebreak")];
    const efforts: ReasoningEffort[] = ["low", "medium", "high"];
    useCodeCatalogStore.setState({
      repos,
      doctor: {
        harnesses: [harness("claude_code"), harness("opencode")],
        notices: [],
      } as never,
    });
    const user = userEvent.setup();
    await renderWithRouter(
      <AppContextProvider
        value={app({
          listCodeHarnessModels: vi.fn((kind: HarnessKind) =>
            Promise.resolve(
              kind === "opencode"
                ? {
                    kind: "opencode" as const,
                    models: [
                      {
                        id: "gpt-5.6-sol",
                        label: "GPT 5.6 Sol",
                        default: true,
                        reasoning_efforts: [],
                        fast_mode: false,
                      },
                    ],
                    reasoning_efforts: [],
                    fast_mode: false,
                  }
                : {
                    kind: "claude_code" as const,
                    models: [
                      {
                        id: "claude-opus-5",
                        label: "Claude Opus 5",
                        default: true,
                        reasoning_efforts: [],
                        fast_mode: true,
                      },
                      {
                        id: "claude-sonnet-5",
                        label: "Claude Sonnet 5",
                        default: false,
                        reasoning_efforts: [],
                        fast_mode: false,
                      },
                    ],
                    reasoning_efforts: efforts,
                    fast_mode: true,
                  },
            ),
          ),
        })}
      >
        <NewWorkspaceDialog open onOpenChange={vi.fn()} repos={repos} />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );

    expect(
      await screen.findByRole("button", { name: "Model: Claude Opus 5" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Reasoning: Default" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("switch", { name: "Fast mode off" }),
    ).toBeInTheDocument();

    await user.click(
      screen.getByRole("button", { name: "Model: Claude Opus 5" }),
    );
    await user.click(screen.getByRole("menuitem", { name: /Claude Sonnet 5/ }));
    expect(
      screen.queryByRole("switch", { name: /Fast mode/ }),
    ).not.toBeInTheDocument();

    await user.click(
      screen.getByRole("button", { name: "Harness: Claude Code" }),
    );
    await user.click(screen.getByRole("menuitem", { name: /opencode/ }));
    expect(
      await screen.findByRole("button", { name: "Model: GPT 5.6 Sol" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /Reasoning:/ }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("switch", { name: /Fast mode/ }),
    ).not.toBeInTheDocument();
  });

  it("shows fast mode on a gateway catalog row the harness listing marks as fast", async () => {
    const repos = [repo("repo-new", "tidebreak")];
    useCodeCatalogStore.setState({
      repos,
      doctor: {
        harnesses: [harness("claude_code")],
        notices: [],
      } as never,
    });
    const listCodeHarnessModels = vi.fn(async () => ({
      kind: "claude_code" as const,
      models: [
        {
          id: "claude-opus-5",
          label: "Claude Opus 5",
          default: true,
          reasoning_efforts: [],
          fast_mode: true,
        },
      ],
      reasoning_efforts: ["low", "medium", "high"] as ReasoningEffort[],
      fast_mode: true,
    }));
    await renderWithRouter(
      <AppContextProvider
        value={{
          ...app({ listCodeHarnessModels }),
          models: [
            {
              key: "model_gateway::claude-opus-5",
              id: "claude-opus-5",
              display_name: "Claude Opus 5",
              provider: "model_gateway",
              vendor: "anthropic",
              available: true,
            } as never,
          ],
          defaultModelKey: "model_gateway::claude-opus-5",
        }}
      >
        <NewWorkspaceDialog open onOpenChange={vi.fn()} repos={repos} />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );

    expect(listCodeHarnessModels).toHaveBeenCalledWith("claude_code");
    expect(
      await screen.findByRole("button", { name: "Model: Claude Opus 5" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("switch", { name: "Fast mode off" }),
    ).toBeInTheDocument();
  });

  it("posts the chosen reasoning effort and fast mode on create", async () => {
    const repos = [repo("repo-new", "tidebreak")];
    const efforts: ReasoningEffort[] = ["low", "medium", "high"];
    useCodeCatalogStore.setState({
      repos,
      doctor: {
        harnesses: [harness("claude_code")],
        notices: [],
      } as never,
    });
    const createCodeWorkspace = vi.fn(async () =>
      workspace("ws-settings", "repo-new", "2026-08-20T00:00:00.000Z"),
    );
    const createCodeSession = vi.fn(async () =>
      session("ws-settings", "claude_code", "2026-08-20T00:00:00.000Z"),
    );
    const user = userEvent.setup();
    await renderWithRouter(
      <AppContextProvider
        value={app({
          createCodeWorkspace,
          createCodeSession,
          listCodeHarnessModels: vi.fn(async () => ({
            kind: "claude_code" as const,
            models: [
              {
                id: "claude-opus-5",
                label: "Claude Opus 5",
                default: true,
                reasoning_efforts: [],
                fast_mode: true,
              },
            ],
            reasoning_efforts: efforts,
            fast_mode: true,
          })),
        })}
      >
        <NewWorkspaceDialog open onOpenChange={vi.fn()} repos={repos} />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );

    expect(
      await screen.findByRole("button", { name: "Model: Claude Opus 5" }),
    ).toBeInTheDocument();
    await user.click(
      screen.getByRole("button", { name: "Reasoning: Default" }),
    );
    await user.click(screen.getByRole("menuitem", { name: "High" }));
    await user.click(screen.getByRole("switch", { name: "Fast mode off" }));
    fireEvent.keyDown(screen.getByRole("dialog"), {
      key: "Enter",
      metaKey: true,
    });
    await waitFor(() =>
      expect(createCodeSession).toHaveBeenCalledWith("ws-settings", {
        harness: "claude_code",
        permission_mode: "allow",
        model: "claude-opus-5",
        reasoning_effort: "high",
        fast_mode: true,
      }),
    );
    await waitFor(() =>
      expect(useCodeUiStore.getState().lastCreate).toEqual({
        repoId: "repo-new",
        harness: "claude_code",
        modelsByHarness: { claude_code: "claude-opus-5" },
        permissionMode: "allow",
        reasoningEffortByHarness: { claude_code: "high" },
        fastModeByHarness: { claude_code: true },
      }),
    );
  });

  it("restores lastCreate effort and fast mode only when the engine still honors them", async () => {
    const repos = [repo("repo-new", "tidebreak")];
    const efforts: ReasoningEffort[] = ["low", "medium", "high"];
    useCodeCatalogStore.setState({
      repos,
      doctor: {
        harnesses: [harness("grok"), harness("opencode")],
        notices: [],
      } as never,
    });
    useCodeUiStore.setState({
      lastCreate: {
        harness: "grok",
        modelsByHarness: { grok: "grok-4.6" },
        reasoningEffortByHarness: { grok: "high", opencode: "high" },
        fastModeByHarness: { grok: true, opencode: true },
      },
    });
    await renderWithRouter(
      <AppContextProvider
        value={app({
          listCodeHarnessModels: vi.fn((kind: HarnessKind) =>
            Promise.resolve(
              kind === "opencode"
                ? {
                    kind: "opencode" as const,
                    models: [
                      {
                        id: "gpt-5.6-sol",
                        label: "GPT 5.6 Sol",
                        default: true,
                        reasoning_efforts: [],
                        fast_mode: false,
                      },
                    ],
                    reasoning_efforts: [],
                    fast_mode: false,
                  }
                : {
                    kind: "grok" as const,
                    models: [
                      {
                        id: "grok-4.6",
                        label: "Grok 4.6",
                        default: true,
                        reasoning_efforts: efforts,
                        fast_mode: false,
                      },
                    ],
                    reasoning_efforts: efforts,
                    fast_mode: false,
                  },
            ),
          ),
        })}
      >
        <NewWorkspaceDialog open onOpenChange={vi.fn()} repos={repos} />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );

    expect(
      await screen.findByRole("button", { name: "Model: Grok 4.6" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Reasoning: High" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("switch", { name: /Fast mode/ }),
    ).not.toBeInTheDocument();

    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Harness: Grok CLI" }));
    await user.click(screen.getByRole("menuitem", { name: /opencode/ }));
    expect(
      await screen.findByRole("button", { name: "Model: GPT 5.6 Sol" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /Reasoning:/ }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("switch", { name: /Fast mode/ }),
    ).not.toBeInTheDocument();
  });
});
