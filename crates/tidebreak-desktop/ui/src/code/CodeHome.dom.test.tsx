// @vitest-environment jsdom
import { act, cleanup, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { AppContextProvider, type AppContextValue } from "@/AppContext";
import { renderWithRouter } from "@/test/router";
import type {
  CodeRepoSnapshot,
  CodeWorkspaceSnapshot,
  HarnessDoctorEntry,
  HarnessDoctorReport,
} from "../api/types";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { CodeHome } from "./CodeHome";
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
      },
      commands: [],
      auth_mode: "local_sign_in",
      remediation: "",
      stderr: "",
      unrecognized_event_count: 0,
      relaunch_composes_permission_mode: true,
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
    const main = document.querySelector(".main");
    expect(main).not.toBeNull();
    expect(
      within(main as HTMLElement).getByRole("button", { name: "Add repo" }),
    ).toBeInTheDocument();
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });

  it("leaves the loading empty when the doctor request fails", async () => {
    await renderHome(
      app({
        getHarnessDoctor: vi.fn(async () => {
          throw new Error("doctor unavailable");
        }),
      }),
    );

    expect(
      await screen.findByRole("heading", { name: "Set up a coding engine" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByText("Start with a repository"),
    ).not.toBeInTheDocument();
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
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
      await screen.findByRole("heading", { name: "Repos" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByText("Start with a repository"),
    ).not.toBeInTheDocument();
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });
});
