// @vitest-environment jsdom
import { cleanup, fireEvent, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { AppContextProvider, type AppContextValue } from "@/AppContext";
import { renderWithRouter } from "@/test/router";
import { AddRepoPalette } from "./AddRepoPalette";
import { useCodeUpdatesStore } from "./CodeUpdatesStore";

afterEach(() => {
  cleanup();
  useCodeUpdatesStore.getState().reset();
});

function app(overrides: Partial<AppContextValue["client"]> = {}): AppContextValue {
  return {
    client: {
      getCodeCloneDefaults: vi.fn(async () => ({
        parent_dir: "/tmp/src",
        gh_found: false,
        gh_remediation: "gh is not installed. Install the GitHub CLI.",
      })),
      startCodeClone: vi.fn(),
      getCodeRepo: vi.fn(),
      createCodeRepo: vi.fn(),
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
    restartForUpdate: async () => {},
  };
}

async function renderPalette(value: AppContextValue = app()) {
  return renderWithRouter(
    <AppContextProvider value={value}>
      <AddRepoPalette open onOpenChange={vi.fn()} />
    </AppContextProvider>,
    { initialUrl: "/code" },
  );
}

describe("AddRepoPalette", () => {
  it("filters sources and selects with the keyboard", async () => {
    await renderPalette();
    const search = await screen.findByPlaceholderText("Filter sources");
    fireEvent.change(search, { target: { value: "git" } });
    expect(screen.getByRole("option", { name: /Git URL/ })).toBeInTheDocument();
    expect(screen.queryByRole("option", { name: /Local folder/ })).not.toBeInTheDocument();
    fireEvent.keyDown(search, { key: "Enter" });
    expect(await screen.findByText("Clone from a remote URL into a parent folder.")).toBeInTheDocument();
  });

  it("goes back a stage on Backspace and closes on Escape", async () => {
    const onOpenChange = vi.fn();
    await renderWithRouter(
      <AppContextProvider value={app()}>
        <AddRepoPalette open onOpenChange={onOpenChange} />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );
    fireEvent.click(await screen.findByRole("option", { name: /GitHub repository/ }));
    expect(await screen.findByText("Clone an owner/repo from GitHub.")).toBeInTheDocument();
    fireEvent.keyDown(screen.getByRole("dialog"), { key: "Backspace" });
    expect(await screen.findByRole("option", { name: /Local folder/ })).toBeInTheDocument();
    fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });
    expect(onOpenChange).toHaveBeenCalledWith(false);
  });

  it("shows the gh-absent hint on the GitHub form", async () => {
    await renderPalette();
    fireEvent.click(await screen.findByRole("option", { name: /GitHub repository/ }));
    expect(await screen.findByTestId("gh-absent-hint")).toHaveTextContent(
      "gh is not installed",
    );
  });

  it("drives clone progress to done and shows an error tail with retry", async () => {
    const started = vi.fn(async () => ({
      id: "job-1",
      phase: "starting",
      done: false,
    }));
    const getRepo = vi.fn(async () => ({
      id: "repo-1",
      root_path: "/tmp/src/demo",
      display_name: "demo",
      default_base_ref: "main",
      branch_prefix: "tidebreak/",
      quick_actions: [],
      created_at: "2026-08-17T00:00:00.000Z",
    }));
    await renderPalette(
      app({
        startCodeClone: started,
        getCodeRepo: getRepo,
      }),
    );
    fireEvent.click(await screen.findByRole("option", { name: /Git URL/ }));
    fireEvent.change(screen.getByPlaceholderText("https://example.com/acme/app.git"), {
      target: { value: "/tmp/origin.git" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Clone" }));
    await waitFor(() => expect(started).toHaveBeenCalled());
    expect(await screen.findByTestId("clone-phase")).toHaveTextContent("starting");
    useCodeUpdatesStore.getState().apply({
      type: "clone_progress",
      job: { id: "job-1", phase: "receiving objects", percent: 40, done: false },
    });
    await waitFor(() =>
      expect(screen.getByTestId("clone-phase")).toHaveTextContent("receiving objects"),
    );
    useCodeUpdatesStore.getState().apply({
      type: "clone_progress",
      job: {
        id: "job-1",
        phase: "failed",
        done: true,
        error: "fatal: repository not found",
      },
    });
    expect(await screen.findByText("fatal: repository not found")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(screen.getByPlaceholderText("https://example.com/acme/app.git")).toBeInTheDocument();
  });
});
