// @vitest-environment jsdom
import { cleanup, fireEvent, screen, waitFor } from "@testing-library/react";
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
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { useCodeUiStore } from "./CodeUiStore";
import { NewWorkspaceDialog } from "./NewWorkspaceDialog";

afterEach(() => {
  cleanup();
  useCodeCatalogStore.getState().reset();
  useCodeUiStore.setState({ lastCreate: null });
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
    tier: "reference",
    caps: { ...CAPS },
    commands: [],
    remediation: "",
    stderr: "",
    unrecognized_event_count: 0,
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
    restartForUpdate: async () => {},
  } as AppContextValue;
}

describe("NewWorkspaceDialog", () => {
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
            models: [{ id: "gpt-5.6-sol", label: "GPT 5.6 Sol", default: true }],
          })),
        })}
      >
        <NewWorkspaceDialog open onOpenChange={vi.fn()} repos={repos} />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );

    const repoField = screen.getByRole("combobox", { name: "Repo" });
    expect(repoField).toHaveTextContent("tidebreak");
    expect(repoField).toHaveFocus();
    expect(repoField).toBeEnabled();
    expect(screen.getByRole("combobox", { name: "Harness" })).toHaveTextContent(
      "Codex CLI",
    );
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Model: GPT 5.6 Sol" }),
      ).toBeEnabled(),
    );
    // Allow is the default where the engine honors it, and it says so.
    expect(
      screen.getByRole("combobox", { name: "Permission mode" }),
    ).toHaveTextContent("Allow all");
    expect(
      screen.getByText(/every action runs without asking/),
    ).toBeInTheDocument();

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
        model: "gpt-5.6-sol",
      }),
    );
  });

  it("lists the workspace block before harness, then model", async () => {
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
          listCodeHarnessModels: vi.fn(async () => ({
            kind: "claude_code" as const,
            models: [{ id: "sonnet", label: "Sonnet", default: true }],
          })),
        })}
      >
        <NewWorkspaceDialog open onOpenChange={vi.fn()} repos={repos} />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );

    const labels = [
      ...screen
        .getByRole("dialog")
        .querySelectorAll(
          "form > div > span.font-medium, form > label > span.font-medium",
        ),
    ].map((el) => el.textContent);
    expect(labels).toEqual([
      "Repo",
      "Title",
      "Base ref",
      "Harness",
      "Model",
      "Permission mode",
    ]);
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
          listCodeHarnessModels: vi.fn(async () => ({
            kind: "claude_code" as const,
            models: [{ id: "sonnet", label: "Sonnet", default: true }],
          })),
        })}
      >
        <NewWorkspaceDialog
          open
          onOpenChange={vi.fn()}
          repos={repos}
          defaultRepoId="repo-old"
        />
      </AppContextProvider>,
      { initialUrl: "/code/r/repo-old" },
    );

    const repoField = screen.getByRole("combobox", { name: "Repo" });
    expect(repoField).toHaveTextContent("legacy");
    expect(repoField).toBeEnabled();
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
              { id: "gpt-5.6-sol", label: "GPT 5.6 Sol", default: true },
              { id: "claude-opus-5", label: "Claude Opus 5", default: false },
              { id: "grok-4.5", label: "Grok 4.5", default: false },
            ],
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
    expect(screen.getByRole("menuitem", { name: /Claude Opus 5/ })).toBeInTheDocument();
    expect(
      screen.queryByRole("menuitem", { name: /GPT 5.6 Sol/ }),
    ).not.toBeInTheDocument();
  });
});
