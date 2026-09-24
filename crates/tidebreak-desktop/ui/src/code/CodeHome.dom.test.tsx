// @vitest-environment jsdom
import {
  act,
  cleanup,
  fireEvent,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { AppContextProvider, type AppContextValue } from "@/AppContext";
import { renderWithRouter } from "@/test/router";
import type {
  Attention,
  CodeRepoSnapshot,
  CodeSessionDigest,
  CodeWorkspaceSnapshot,
  HarnessDoctorEntry,
  HarnessDoctorReport,
} from "../api/types";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { CodeHome } from "./CodeHome";
import { useEngineSignInStore } from "./EngineSignIn";
import { disconnectCodeUpdates, useCodeUpdatesStore } from "./CodeUpdatesStore";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((next) => {
    resolve = next;
  });
  return { promise, resolve: (value: T) => resolve(value) };
}

const REPO: CodeRepoSnapshot = {
  id: "repo-1",
  root_path: "/tmp/app",
  display_name: "app",
  default_base_ref: "main",
  branch_prefix: "tidebreak",
  quick_actions: [],
  created_at: "2026-08-15T00:00:00.000Z",
};

const READY_DOCTOR: HarnessDoctorReport = {
  harnesses: [
    {
      kind: "claude_code",
      found: true,
      installable: true,
      authenticated: true,
      tier: "reference",
      caps: {
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
        durable_parks: "unsupported",
        user_questions: "unsupported",
        standing_grants: "unsupported",
        mid_turn_resume: "unsupported",
        transcript: "unsupported",
        memory_loopback: "unsupported",
      },
      commands: [],
      auth_mode: "local_sign_in",
      remediation: "",
      stderr: "",
      unrecognized_event_count: 0,
      relaunch_composes_permission_mode: true,
      update_available: false,
    } as HarnessDoctorEntry,
  ],
};

function app(
  overrides: Partial<AppContextValue["client"]> = {},
): AppContextValue {
  return {
    client: {
      listCodeRepos: vi.fn(async () => []),
      listCodeWorkspaces: vi.fn(async () => []),
      getHarnessDoctor: vi.fn(async () => READY_DOCTOR),
      listCodeHarnessModels: vi.fn(async () => ({
        kind: "claude_code" as const,
        models: [],
      })),
      getCodeCloneDefaults: vi.fn(async () => ({
        parent_dir: "/tmp/src",
        gh_found: false,
        gh_remediation: "gh is not installed.",
      })),
      openCodeUpdates: vi.fn(() => {
        return {
          close() {},
          addEventListener() {},
          removeEventListener() {},
        } as unknown as WebSocket;
      }),
      ...overrides,
    } as never,
    models: [],
    defaultModelKey: null,
    providers: [],
    refreshCatalog: async () => {},
    refreshChats: async () => {},
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

async function renderHome(value: AppContextValue = app()) {
  return renderWithRouter(
    <AppContextProvider value={value}>
      <CodeHome />
    </AppContextProvider>,
    { initialUrl: "/code" },
  );
}

afterEach(() => {
  cleanup();
  useCodeCatalogStore.getState().reset();
  disconnectCodeUpdates();
  useCodeUpdatesStore.getState().reset();
});

describe("CodeHome", () => {
  it("shows a loading empty state before the catalog resolves", async () => {
    const repos = deferred<CodeRepoSnapshot[]>();
    await renderHome(
      app({
        listCodeRepos: vi.fn(() => repos.promise),
        listCodeWorkspaces: vi.fn(async () => []),
        getHarnessDoctor: vi.fn(
          () => new Promise<HarnessDoctorReport>(() => {}),
        ),
      }),
    );

    expect(screen.getByRole("status")).toHaveTextContent("Loading…");
    expect(
      screen.queryByText("Start with a repository"),
    ).not.toBeInTheDocument();
  });

  it("keeps loading after empty repos until the doctor arrives", async () => {
    const repos = deferred<CodeRepoSnapshot[]>();
    const workspaces = deferred<CodeWorkspaceSnapshot[]>();
    const doctor = deferred<HarnessDoctorReport>();
    await renderHome(
      app({
        listCodeRepos: vi.fn(() => repos.promise),
        listCodeWorkspaces: vi.fn(() => workspaces.promise),
        getHarnessDoctor: vi.fn(() => doctor.promise),
      }),
    );

    await act(async () => {
      repos.resolve([]);
      workspaces.resolve([]);
    });

    expect(screen.getByRole("status")).toHaveTextContent("Loading…");
    expect(
      screen.queryByText("Start with a repository"),
    ).not.toBeInTheDocument();
  });

  it("shows the empty state once repos are empty and a harness is ready", async () => {
    await renderHome();

    expect(
      await screen.findByRole("heading", { name: "Start with a repository" }),
    ).toBeInTheDocument();
    // The page no longer mounts the rail — the code layout route owns it —
    // so the page's own Add repo button is the only one here.
    expect(
      screen.getByRole("button", { name: "Add repo" }),
    ).toBeInTheDocument();
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });

  it("does not count the workspace-less internal engine as a coding engine", async () => {
    const external = {
      ...READY_DOCTOR.harnesses[0],
      authenticated: false,
    } as HarnessDoctorEntry;
    const internal = {
      ...READY_DOCTOR.harnesses[0],
      kind: "internal",
      installable: false,
    } as HarnessDoctorEntry;
    await renderHome(
      app({
        getHarnessDoctor: vi.fn(async () => ({
          harnesses: [internal, external],
        })),
      }),
    );

    expect(
      await screen.findByRole("heading", { name: "Set up a coding engine" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("heading", { name: "Start with a repository" }),
    ).not.toBeInTheDocument();
  });

  // A download alone does not sign an engine in. A fresh machine used to see
  // the repo form here and met the sign-in when its first turn failed.
  it("shows the doctor on a fresh machine instead of the repo form", async () => {
    const notDownloaded = {
      ...READY_DOCTOR.harnesses[0],
      found: false,
      authenticated: undefined,
      sign_in_command: "claude auth login",
    } as HarnessDoctorEntry;
    await renderHome(
      app({
        getHarnessDoctor: vi.fn(async () => ({ harnesses: [notDownloaded] })),
      }),
    );

    expect(
      await screen.findByRole("heading", { name: "Set up a coding engine" }),
    ).toBeInTheDocument();
    expect(
      screen.getByText(
        "No engine is ready yet. Download one below, then sign in to it.",
      ),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /Download/ }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("heading", { name: "Start with a repository" }),
    ).not.toBeInTheDocument();
  });

  it("opens the engine's sign-in from its doctor row", async () => {
    const signedOut = {
      ...READY_DOCTOR.harnesses[0],
      authenticated: false,
      sign_in_command: "claude auth login",
    } as HarnessDoctorEntry;
    useEngineSignInStore.setState({ kind: null });
    await renderHome(
      app({
        getHarnessDoctor: vi.fn(async () => ({ harnesses: [signedOut] })),
      }),
    );

    await userEvent.click(
      await screen.findByRole("button", { name: "Sign in to Claude Code" }),
    );
    expect(useEngineSignInStore.getState().kind).toBe("claude_code");
    useEngineSignInStore.setState({ kind: null });
  });

  // opencode with no stored key can still run a local model, so an engine
  // whose sign-in Tidebreak could not confirm does not hold the page.
  it("lets an unconfirmed engine through to the repo form", async () => {
    const unconfirmed = {
      ...READY_DOCTOR.harnesses[0],
      kind: "opencode",
      authenticated: undefined,
    } as HarnessDoctorEntry;
    await renderHome(
      app({
        getHarnessDoctor: vi.fn(async () => ({ harnesses: [unconfirmed] })),
      }),
    );

    expect(
      await screen.findByRole("heading", { name: "Start with a repository" }),
    ).toBeInTheDocument();
  });

  it("keeps Add repo available and retries when the engine check fails", async () => {
    const refreshHarnessDoctor = vi.fn(async () => READY_DOCTOR);
    await renderHome(
      app({
        getHarnessDoctor: vi.fn(async () => {
          throw new Error("doctor unavailable");
        }),
        refreshHarnessDoctor,
      }),
    );

    expect(
      await screen.findByText(
        "The coding engine check did not answer: doctor unavailable",
      ),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("heading", { name: "Start with a repository" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Add repo" })).toBeVisible();
    await userEvent.click(screen.getByRole("button", { name: "Re-check" }));
    expect(refreshHarnessDoctor).toHaveBeenCalledOnce();
  });

  it("offers Try again when the repository catalog fails", async () => {
    const listCodeRepos = vi
      .fn()
      .mockRejectedValueOnce(new Error("catalog unavailable"))
      .mockResolvedValueOnce([]);
    await renderHome(app({ listCodeRepos }));

    expect(await screen.findByText("catalog unavailable")).toBeVisible();
    await userEvent.click(screen.getByRole("button", { name: "Try again" }));
    await waitFor(() => expect(listCodeRepos).toHaveBeenCalledTimes(2));
  });

  it("lists repos as soon as the catalog loads, without waiting for the doctor", async () => {
    const repos = deferred<CodeRepoSnapshot[]>();
    await renderHome(
      app({
        listCodeRepos: vi.fn(() => repos.promise),
        listCodeWorkspaces: vi.fn(async () => []),
        getHarnessDoctor: vi.fn(
          () => new Promise<HarnessDoctorReport>(() => {}),
        ),
      }),
    );

    await act(async () => {
      repos.resolve([REPO]);
    });

    expect(
      await screen.findByRole("heading", { name: /^Repositories/ }),
    ).toBeInTheDocument();
    expect(
      screen.queryByText("Start with a repository"),
    ).not.toBeInTheDocument();
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });
});

const approval: Attention = {
  state: {
    type: "needs_you",
    prompt: "an approval is waiting",
    source: "structured",
  },
  source: "structured",
};

function workspace(
  id: string,
  title: string,
  overrides: Partial<CodeWorkspaceSnapshot> = {},
): CodeWorkspaceSnapshot {
  return {
    id,
    repo_id: REPO.id,
    title,
    worktree_path: `/tmp/worktrees/${id}`,
    branch_name: `tidebreak/${id}`,
    base_ref: "main",
    status: "active",
    created_at: "2026-09-01T00:00:00.000Z",
    ...overrides,
  };
}

function digest(
  workspaceId: string | null,
  overrides: Partial<CodeSessionDigest> = {},
): CodeSessionDigest {
  return {
    workspace: workspaceId,
    session: `sess-${workspaceId}`,
    kind: "interactive",
    lifecycle: "idle",
    attention: { state: { type: "done_unreviewed" }, source: "lifecycle" },
    title: "",
    turn_count: 2,
    trigger_target_at: "2026-09-20T10:00:00.000Z",
    ...overrides,
  };
}

/** What the rail's socket delivers on connect; the home only reads it. */
function deliverSnapshot(sessions: CodeSessionDigest[]) {
  act(() => {
    useCodeUpdatesStore.getState().apply({ type: "snapshot", sessions });
  });
}

function section(name: RegExp) {
  const region = screen.getByRole("region", { name });
  return within(region);
}

describe("CodeHome for a returning reader", () => {
  const WORKSPACES = [
    workspace("ws-ask", "Answer the approval"),
    workspace("ws-suite", "Run the suite"),
    workspace("ws-fix", "Ship the fix", {
      pr: {
        number: 41,
        url: "https://github.com/acme/app/pull/41",
        state: "open",
        title: "Ship the fix",
        mergeable: "mergeable",
        merge_state_status: "clean",
        check_counts: { passing: 3, pending: 0, failing: 0, skipped: 0 },
      },
    }),
    workspace("ws-idea", "Parked idea"),
    workspace("ws-shelved", "Put away", { status: "archived" }),
  ];
  const SESSIONS = [
    digest("ws-ask", { attention: approval }),
    digest("ws-suite", {
      lifecycle: "running",
      attention: { state: { type: "working" }, source: "lifecycle" },
    }),
    digest(null, {
      session: "sess-slack",
      title: "Triage the alert",
      attention: approval,
    }),
  ];

  async function renderReturning(
    overrides: Partial<AppContextValue["client"]> = {},
  ) {
    const rendered = await renderHome(
      app({
        listCodeRepos: vi.fn(async () => [REPO]),
        listCodeWorkspaces: vi.fn(async () => WORKSPACES),
        ...overrides,
      }),
    );
    await screen.findByRole("heading", { name: /^Repositories/ });
    return rendered;
  }

  it("leads with needs, running work, ready pull requests, and recent work", async () => {
    await renderReturning();
    deliverSnapshot(SESSIONS);

    const headings = screen
      .getAllByRole("heading", { level: 2 })
      .map((heading) => heading.textContent);
    expect(headings).toEqual([
      "Needs you2",
      "Running1",
      "Ready to merge1",
      "Recent work1",
      "Repositories1",
    ]);
    expect(
      section(/^Needs you/).getByRole("button", {
        name: /^Answer the approval · An approval is waiting · app/,
      }),
    ).toBeInTheDocument();
    expect(
      section(/^Needs you/).getByRole("button", { name: /^Triage the alert/ }),
    ).toBeInTheDocument();
    expect(
      section(/^Running/).getByRole("button", { name: /^Run the suite/ }),
    ).toBeInTheDocument();
    expect(
      section(/^Ready to merge/).getByRole("button", {
        name: /^Ship the fix · 3 checks passed · app · #41$/,
      }),
    ).toBeInTheDocument();
    expect(
      section(/^Recent work/).getByRole("button", { name: /^Parked idea/ }),
    ).toBeInTheDocument();
    expect(screen.queryByText("Put away")).not.toBeInTheDocument();
  });

  it("opens the workspace, the conversation, and the pull request in Delivery", async () => {
    const { router } = await renderReturning();
    deliverSnapshot(SESSIONS);

    await userEvent.click(
      screen.getByRole("button", { name: /^Answer the approval/ }),
    );
    expect(router.state.location.pathname).toBe("/code/w/ws-ask");

    await act(() => router.navigate({ to: "/code" }));
    await userEvent.click(
      await screen.findByRole("button", { name: /^Triage the alert/ }),
    );
    expect(router.state.location.pathname).toBe("/code/s/sess-slack");

    await act(() => router.navigate({ to: "/code" }));
    await userEvent.click(
      await screen.findByRole("button", { name: /^Ship the fix/ }),
    );
    expect(router.state.location.pathname).toBe("/code/delivery/pull-requests");
    expect(router.state.location.search).toEqual({
      repoHost: "github.com",
      repoOwner: "acme",
      repoName: "app",
      pr: 41,
    });
  });

  it("shows five items a section and the rest behind View all", async () => {
    const many = Array.from({ length: 7 }, (_, index) =>
      workspace(`ws-need-${index + 1}`, `Need ${index + 1}`),
    );
    await renderReturning({ listCodeWorkspaces: vi.fn(async () => many) });
    deliverSnapshot(
      many.map((item) => digest(item.id, { attention: approval })),
    );

    const needs = section(/^Needs you/);
    expect(needs.getAllByRole("button", { name: /^Need \d/ })).toHaveLength(5);
    const viewAll = needs.getByRole("button", {
      name: "View all 7 in Needs you",
    });
    expect(viewAll).toHaveAttribute("aria-expanded", "false");
    await userEvent.click(viewAll);
    expect(needs.getAllByRole("button", { name: /^Need \d/ })).toHaveLength(7);
    expect(
      needs.getByRole("button", { name: "Show fewer in Needs you" }),
    ).toHaveAttribute("aria-expanded", "true");
  });

  it("says nothing needs you only once the live snapshot has landed", async () => {
    await renderReturning({
      listCodeWorkspaces: vi.fn(async () => [
        workspace("ws-idea", "Parked idea"),
      ]),
    });
    expect(
      screen.queryByText("Nothing needs you right now."),
    ).not.toBeInTheDocument();

    deliverSnapshot([]);
    expect(screen.getByText("Nothing needs you right now.")).toBeVisible();
  });

  it("invites a first workspace when repositories have none", async () => {
    await renderHome(app({ listCodeRepos: vi.fn(async () => [REPO]) }));
    expect(
      await screen.findByRole("heading", { name: /^Repositories/ }),
    ).toBeInTheDocument();
    expect(screen.getByText("No workspaces yet")).toBeVisible();
    expect(screen.getByRole("button", { name: "New workspace" })).toBeVisible();
  });

  it("keeps repository settings in the row menu, not a button per row", async () => {
    await renderReturning({
      getCodeRepo: vi.fn(() => new Promise<CodeRepoSnapshot>(() => {})),
      getCodeRepoTrust: vi.fn(() => new Promise<never>(() => {})),
    });
    const repositories = section(/^Repositories/);
    expect(
      repositories.queryByRole("button", { name: "Settings" }),
    ).not.toBeInTheDocument();

    const row = repositories.getByRole("button", {
      name: "New workspace on app",
    });
    fireEvent.contextMenu(row);
    await userEvent.click(
      await screen.findByRole("menuitem", { name: "Repository settings…" }),
    );
    expect(
      await screen.findByRole("dialog", { name: "Repository settings" }),
    ).toBeInTheDocument();
  });

  it("opens the same row menu from the keyboard", async () => {
    await renderReturning();
    const row = section(/^Repositories/).getByRole("button", {
      name: "New workspace on app",
    });
    row.focus();
    fireEvent.keyDown(row, { key: "F10", shiftKey: true });
    expect(
      await screen.findByRole("menuitem", { name: "Repository settings…" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("menuitem", { name: "Copy path" })).toBeVisible();
  });

  it("keeps the whole repository path and its tail intact for middle truncation", async () => {
    const path =
      "/Users/sam/src/brightwave/product-foundations/design-system-components";
    await renderReturning({
      listCodeRepos: vi.fn(async () => [{ ...REPO, root_path: path }]),
    });
    const shown = section(/^Repositories/).getByTitle(path);
    // Every character is in the row; CSS clips the head and never the tail.
    expect(shown).toHaveTextContent(path);
    expect(shown.lastElementChild).toHaveTextContent(
      /design-system-components$/,
    );
  });
});
