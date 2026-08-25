// @vitest-environment jsdom
import { cleanup, fireEvent, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { AppContextProvider, type AppContextValue } from "@/AppContext";
import { renderWithRouter } from "@/test/router";
import { setAttachedRemotely } from "@/host";
import { AddRepoPalette } from "./AddRepoPalette";
import { useCodeUpdatesStore } from "./CodeUpdatesStore";

afterEach(() => {
  cleanup();
  useCodeUpdatesStore.getState().reset();
});

function app(
  overrides: Partial<AppContextValue["client"]> = {},
): AppContextValue {
  return {
    client: {
      getCodeCloneDefaults: vi.fn(async () => ({
        parent_dir: "/tmp/src",
        gh_found: false,
        gh_remediation: "gh is not installed. Install the GitHub CLI.",
      })),
      getCodeRepoSources: vi.fn(async () => ({
        sources: [
          { kind: "local", available: true },
          { kind: "git_url", available: true },
          { kind: "github", available: true },
        ],
        chooses_destination: false,
      })),
      listCodeGithubRepositories: vi.fn(async () => ({ repositories: [] })),
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
    attachment: "local",
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
    expect(
      screen.queryByRole("option", { name: /Local folder/ }),
    ).not.toBeInTheDocument();
    fireEvent.keyDown(search, { key: "Enter" });
    expect(
      await screen.findByText("Clone from a remote URL into a parent folder."),
    ).toBeInTheDocument();
  });

  it("goes back a stage on Backspace and closes on Escape", async () => {
    const onOpenChange = vi.fn();
    await renderWithRouter(
      <AppContextProvider value={app()}>
        <AddRepoPalette open onOpenChange={onOpenChange} />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );
    fireEvent.click(
      await screen.findByRole("option", { name: /GitHub repository/ }),
    );
    expect(
      await screen.findByText("Clone an owner/repo from GitHub."),
    ).toBeInTheDocument();
    fireEvent.keyDown(screen.getByRole("dialog"), { key: "Backspace" });
    expect(
      await screen.findByRole("option", { name: /Local folder/ }),
    ).toBeInTheDocument();
    fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });
    expect(onOpenChange).toHaveBeenCalledWith(false);
  });

  it("shows the gh-absent hint on the GitHub form", async () => {
    await renderPalette();
    fireEvent.click(
      await screen.findByRole("option", { name: /GitHub repository/ }),
    );
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
    fireEvent.change(
      screen.getByPlaceholderText("https://example.com/acme/app.git"),
      {
        target: { value: "/tmp/origin.git" },
      },
    );
    fireEvent.click(screen.getByRole("button", { name: "Clone" }));
    await waitFor(() => expect(started).toHaveBeenCalled());
    expect(await screen.findByTestId("clone-phase")).toHaveTextContent(
      "starting",
    );
    useCodeUpdatesStore.getState().apply({
      type: "clone_progress",
      job: {
        id: "job-1",
        phase: "receiving objects",
        percent: 40,
        done: false,
      },
    });
    await waitFor(() =>
      expect(screen.getByTestId("clone-phase")).toHaveTextContent(
        "receiving objects",
      ),
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
    expect(
      await screen.findByText("fatal: repository not found"),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(
      screen.getByPlaceholderText("https://example.com/acme/app.git"),
    ).toBeInTheDocument();
  });
});

describe("AddRepoPalette on a machine that answers for itself", () => {
  it("hides a source the machine cannot serve and says what would fix it", async () => {
    await renderPalette(
      app({
        getCodeRepoSources: vi.fn(async () => ({
          sources: [
            { kind: "local", available: true },
            {
              kind: "git_url",
              available: false,
              remediation: "This machine has no git.",
            },
            {
              kind: "github",
              available: false,
              remediation: "This machine has no git.",
            },
          ],
          chooses_destination: false,
        })),
      } as never),
    );
    await waitFor(() =>
      expect(
        screen.queryByRole("option", { name: /Git URL/ }),
      ).not.toBeInTheDocument(),
    );
    expect(
      screen.queryByRole("option", { name: /GitHub repository/ }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByRole("option", { name: /Local folder/ }),
    ).toBeInTheDocument();
    // Absent with no reason reads as a broken dialog; the machine's own
    // sentence is what makes it legible.
    expect(
      screen.getAllByText(/This machine has no git\./).length,
    ).toBeGreaterThan(0);
  });

  it("asks for no destination when the machine places clones itself", async () => {
    const startCodeClone = vi.fn(async (_body: { parent_dir?: string }) => ({
      id: "job-1",
      phase: "starting",
      done: false,
    }));
    await renderPalette(
      app({
        startCodeClone,
        getCodeRepoSources: vi.fn(async () => ({
          sources: [
            { kind: "local", available: true },
            { kind: "git_url", available: true },
            { kind: "github", available: true },
          ],
          chooses_destination: true,
        })),
      } as never),
    );
    const search = await screen.findByPlaceholderText("Filter sources");
    fireEvent.change(search, { target: { value: "Git URL" } });
    fireEvent.keyDown(search, { key: "Enter" });

    const url = await screen.findByPlaceholderText(
      "https://example.com/acme/app.git",
    );
    await waitFor(() =>
      expect(screen.queryByText("Destination folder")).not.toBeInTheDocument(),
    );
    fireEvent.change(url, {
      target: { value: "https://example.com/acme/app.git" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Clone" }));
    await waitFor(() => expect(startCodeClone).toHaveBeenCalled());
    // The machine owns its filesystem layout, so the request names no path.
    expect(startCodeClone.mock.calls[0]?.[0].parent_dir).toBeUndefined();
  });

  it("stays usable when the administrator-only defaults read is refused", async () => {
    await renderPalette(
      app({
        getCodeCloneDefaults: vi.fn(async () => {
          throw new Error("403: forbidden");
        }),
      } as never),
    );
    // A member on a shared machine gets that refusal every time. The dialog
    // is the thing they came for, so it must survive it.
    expect(
      await screen.findByRole("option", { name: /Local folder/ }),
    ).toBeInTheDocument();
  });
});

describe("AddRepoPalette and a machine without a GitHub credential", () => {
  it("still offers owner/repo, and says what the missing credential costs", async () => {
    await renderPalette(
      app({
        getCodeRepoSources: vi.fn(async () => ({
          sources: [
            { kind: "local", available: true },
            { kind: "git_url", available: true },
            {
              kind: "github",
              available: true,
              remediation: "gh is not installed. Install the GitHub CLI.",
            },
          ],
          chooses_destination: false,
        })),
        // Administrator-only, and refused here: the hint must come from the
        // member-plane probe or a member sees nothing.
        getCodeCloneDefaults: vi.fn(async () => {
          throw new Error("403: forbidden");
        }),
      } as never),
    );
    // The clone path falls back to the public HTTPS URL without gh, so
    // hiding this form would take away something that works.
    const github = await screen.findByRole("option", {
      name: /GitHub repository/,
    });
    fireEvent.click(github);
    expect(await screen.findByTestId("gh-absent-hint")).toHaveTextContent(
      /gh is not installed/,
    );
  });
});

describe("AddRepoPalette on a hosted machine that acts as the person", () => {
  it("carries the person attribution sentence on the offered source", async () => {
    await renderPalette(
      app({
        getCodeRepoSources: vi.fn(async () => ({
          sources: [
            { kind: "local", available: true },
            { kind: "git_url", available: true },
            {
              kind: "github",
              available: true,
              remediation:
                "Clones and pushes use your own GitHub account: work lands as mira-chen.",
            },
          ],
          chooses_destination: true,
        })),
        getCodeCloneDefaults: vi.fn(async () => {
          throw new Error("403: forbidden");
        }),
      } as never),
    );
    const github = await screen.findByRole("option", {
      name: /GitHub repository/,
    });
    fireEvent.click(github);
    expect(await screen.findByTestId("gh-absent-hint")).toHaveTextContent(
      /work lands as mira-chen/,
    );
  });

  it("offers the caller's repositories as suggestions", async () => {
    await renderPalette(
      app({
        getCodeRepoSources: vi.fn(async () => ({
          sources: [
            { kind: "local", available: true },
            { kind: "git_url", available: true },
            {
              kind: "github",
              available: true,
              remediation:
                "Clones and pushes use your own GitHub account: work lands as mira-chen.",
            },
          ],
          chooses_destination: true,
        })),
        listCodeGithubRepositories: vi.fn(async () => ({
          repositories: [
            {
              full_name: "mira-chen/notes",
              private: true,
              description: "scratch",
            },
            {
              full_name: "brightwave-inc/tidebreak",
              private: true,
            },
          ],
        })),
        getCodeCloneDefaults: vi.fn(async () => {
          throw new Error("403: forbidden");
        }),
      } as never),
    );
    fireEvent.click(
      await screen.findByRole("option", { name: /GitHub repository/ }),
    );
    fireEvent.click(
      await screen.findByRole("combobox", { name: "Repository" }),
    );
    expect(
      await screen.findByRole("option", { name: /mira-chen\/notes/ }),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("option", { name: /mira-chen\/notes/ }));
    expect(
      screen.getByRole("combobox", { name: "Repository" }),
    ).toHaveTextContent("mira-chen/notes");
  });

  it("clones from the dropdown with Cmd+Enter", async () => {
    const startCodeClone = vi.fn(async () => ({
      id: "job-1",
      phase: "starting",
      done: false,
    }));
    await renderPalette(
      app({
        startCodeClone,
        getCodeRepoSources: vi.fn(async () => ({
          sources: [
            { kind: "local", available: true },
            { kind: "git_url", available: true },
            {
              kind: "github",
              available: true,
              remediation:
                "Clones and pushes use your own GitHub account: work lands as mira-chen.",
            },
          ],
          chooses_destination: true,
        })),
        listCodeGithubRepositories: vi.fn(async () => ({
          repositories: [
            {
              full_name: "mira-chen/notes",
              private: true,
              description: "scratch",
            },
          ],
        })),
        getCodeCloneDefaults: vi.fn(async () => {
          throw new Error("403: forbidden");
        }),
      } as never),
    );
    fireEvent.click(
      await screen.findByRole("option", { name: /GitHub repository/ }),
    );
    fireEvent.click(
      await screen.findByRole("combobox", { name: "Repository" }),
    );
    fireEvent.click(
      await screen.findByRole("option", { name: /mira-chen\/notes/ }),
    );
    fireEvent.keyDown(screen.getByRole("dialog"), {
      key: "Enter",
      metaKey: true,
    });
    await waitFor(() =>
      expect(startCodeClone).toHaveBeenCalledWith(
        expect.objectContaining({ github: "mira-chen/notes" }),
      ),
    );
  });

  it("hides GitHub until the caller connects, and points at the gateway", async () => {
    await renderPalette(
      app({
        getCodeRepoSources: vi.fn(async () => ({
          sources: [
            { kind: "local", available: true },
            { kind: "git_url", available: true },
            {
              kind: "github",
              available: false,
              remediation:
                "To use GitHub as yourself here, connect your GitHub account at the Model Gateway: https://gateway.example/account/apps",
            },
          ],
          chooses_destination: true,
        })),
      } as never),
    );
    await waitFor(() =>
      expect(
        screen.queryByRole("option", { name: /GitHub repository/ }),
      ).not.toBeInTheDocument(),
    );
    expect(
      screen.getByText(/connect your GitHub account at the Model Gateway/),
    ).toBeInTheDocument();
  });
});

describe("AddRepoPalette when attached to a machine with no destination", () => {
  afterEach(() => {
    setAttachedRemotely(false);
  });

  it("hides Local folder and says the destination is missing", async () => {
    setAttachedRemotely(true);
    await renderPalette();
    await waitFor(() =>
      expect(
        screen.queryByRole("option", { name: /Local folder/ }),
      ).not.toBeInTheDocument(),
    );
    fireEvent.click(
      await screen.findByRole("option", { name: /GitHub repository/ }),
    );
    expect(
      await screen.findByTestId("clone-destination-missing"),
    ).toHaveTextContent(/no clone destination configured/);
    expect(screen.getByRole("button", { name: "Clone" })).toBeDisabled();
  });
});
