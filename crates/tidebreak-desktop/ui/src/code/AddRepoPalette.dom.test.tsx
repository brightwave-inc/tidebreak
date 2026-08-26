// @vitest-environment jsdom
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { useState } from "react";
import userEvent from "@testing-library/user-event";
import { toast } from "sonner";
import { afterEach, describe, expect, it, vi } from "vitest";

import { AppContextProvider, type AppContextValue } from "@/AppContext";
import { renderWithRouter } from "@/test/router";
import { setAttachedRemotely } from "@/host";
import { AddRepoPalette } from "./AddRepoPalette";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { useCodeUiStore } from "./CodeUiStore";
import {
  activateCodeCloneClient,
  selectCodeClone,
  trackCodeClone,
  useCodeUpdatesStore,
} from "./CodeUpdatesStore";

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn() },
}));

afterEach(() => {
  cleanup();
  useCodeCatalogStore.getState().reset();
  useCodeUpdatesStore.getState().reset();
  useCodeUiStore.setState({
    newWorkspaceOpen: false,
    newWorkspaceRepoId: undefined,
    addRepoOpen: false,
  });
  setAttachedRemotely(false);
  vi.useRealTimers();
  vi.clearAllMocks();
});

const REPO = {
  id: "repo-1",
  root_path: "/tmp/src/demo",
  display_name: "demo",
  default_base_ref: "main",
  branch_prefix: "tidebreak/",
  quick_actions: [],
  created_at: "2026-08-17T00:00:00.000Z",
};

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((done, fail) => {
    resolve = done;
    reject = fail;
  });
  return { promise, resolve, reject };
}

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
      getCodeCloneJob: vi.fn(() => new Promise(() => {})),
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

function PaletteHarness({
  value,
  mounted = true,
}: {
  value: AppContextValue;
  mounted?: boolean;
}) {
  const [open, setOpen] = useState(true);
  return (
    <AppContextProvider value={value}>
      <button type="button" onClick={() => setOpen(true)}>
        Open add repository
      </button>
      {mounted && <AddRepoPalette open={open} onOpenChange={setOpen} />}
    </AppContextProvider>
  );
}

async function startGitClone(url = "https://example.com/acme/app.git") {
  fireEvent.click(await screen.findByRole("option", { name: /Git URL/ }));
  const urlInput = await screen.findByPlaceholderText(
    "https://example.com/acme/app.git",
  );
  await waitFor(() =>
    expect(screen.getByLabelText("Destination folder")).toHaveValue("/tmp/src"),
  );
  fireEvent.change(urlInput, { target: { value: url } });
  fireEvent.click(screen.getByRole("button", { name: "Clone" }));
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

  it("keeps a destination typed before clone defaults resolve", async () => {
    const defaults =
      deferred<
        Awaited<ReturnType<AppContextValue["client"]["getCodeCloneDefaults"]>>
      >();
    await renderPalette(
      app({
        getCodeCloneDefaults: vi.fn(() => defaults.promise),
      }),
    );
    fireEvent.click(await screen.findByRole("option", { name: /Git URL/ }));
    const destination = await screen.findByLabelText("Destination folder");
    fireEvent.change(destination, { target: { value: "/tmp/chosen" } });

    await act(async () => {
      defaults.resolve({
        parent_dir: "/tmp/stale-default",
        gh_found: true,
        gh_authenticated: true,
        gh_remediation: "",
      });
      await defaults.promise;
    });

    expect(destination).toHaveValue("/tmp/chosen");
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

  it("does not open New Workspace after clone progress is dismissed", async () => {
    const getCodeRepo = vi.fn(async () => REPO);
    const value = app({
      startCodeClone: vi.fn(async () => ({
        id: "job-dismissed",
        phase: "starting",
        done: false,
      })),
      getCodeRepo,
    });
    render(<PaletteHarness value={value} />);

    await startGitClone();
    expect(await screen.findByTestId("clone-phase")).toHaveTextContent(
      "starting",
    );
    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    await waitFor(() =>
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
    );

    act(() => {
      useCodeUpdatesStore.getState().apply({
        type: "clone_progress",
        job: {
          id: "job-dismissed",
          phase: "complete",
          done: true,
          repo_id: REPO.id,
        },
      });
    });

    await waitFor(() => expect(getCodeRepo).not.toHaveBeenCalled());
    expect(useCodeUiStore.getState().newWorkspaceOpen).toBe(false);
  });

  it("opens the exact clone selected by a background notification", async () => {
    const value = app();
    const clientGeneration = activateCodeCloneClient(value.client);
    trackCodeClone(
      value.client,
      { id: "job-a", phase: "Clone A", done: false },
      { github: "acme/a", parent_dir: "/tmp/src" },
      true,
    );
    trackCodeClone(
      value.client,
      { id: "job-b", phase: "Clone B", done: false },
      { github: "acme/b", parent_dir: "/tmp/src" },
      true,
    );
    expect(selectCodeClone({ jobId: "job-a", clientGeneration })).toBe(true);

    render(<PaletteHarness value={value} />);

    expect(await screen.findByTestId("clone-phase")).toHaveTextContent(
      "Clone A",
    );
    expect(screen.queryByText("Clone B")).not.toBeInTheDocument();
  });

  it("keeps the selected clone destination when pending defaults resolve", async () => {
    const defaults =
      deferred<
        Awaited<ReturnType<AppContextValue["client"]["getCodeCloneDefaults"]>>
      >();
    const value = app({
      getCodeCloneDefaults: vi.fn(() => defaults.promise),
    });
    const clientGeneration = activateCodeCloneClient(value.client);
    render(<PaletteHarness value={value} />);
    expect(
      await screen.findByRole("option", { name: /Local folder/ }),
    ).toBeVisible();

    act(() => {
      trackCodeClone(
        value.client,
        { id: "job-a", phase: "Clone A", done: false },
        { github: "acme/a", parent_dir: "/tmp/selected" },
        true,
      );
      selectCodeClone({ jobId: "job-a", clientGeneration });
    });
    expect(await screen.findByTestId("clone-phase")).toHaveTextContent(
      "Clone A",
    );

    await act(async () => {
      defaults.resolve({
        parent_dir: "/tmp/stale-default",
        gh_found: true,
        gh_authenticated: true,
        gh_remediation: "",
      });
      await defaults.promise;
    });
    fireEvent.keyDown(screen.getByRole("dialog"), { key: "Backspace" });

    expect(await screen.findByLabelText("Destination folder")).toHaveValue(
      "/tmp/selected",
    );
  });

  it("aborts every setup probe when the palette closes", async () => {
    const sources = vi.fn((_signal?: AbortSignal) => new Promise(() => {}));
    const github = vi.fn((_signal?: AbortSignal) => new Promise(() => {}));
    const defaults = vi.fn((_signal?: AbortSignal) => new Promise(() => {}));
    render(
      <PaletteHarness
        value={app({
          getCodeRepoSources: sources as never,
          listCodeGithubRepositories: github as never,
          getCodeCloneDefaults: defaults as never,
        })}
      />,
    );
    await waitFor(() => {
      expect(sources).toHaveBeenCalledTimes(1);
      expect(github).toHaveBeenCalledTimes(1);
      expect(defaults).toHaveBeenCalledTimes(1);
    });

    fireEvent.click(screen.getByRole("button", { name: "Close" }));

    for (const probe of [sources, github, defaults]) {
      const signal = probe.mock.calls[0]?.[0];
      expect(signal).toBeInstanceOf(AbortSignal);
      expect(signal?.aborted).toBe(true);
    }
  });

  it("keeps a successful local registration after dismissal", async () => {
    const registration = deferred<typeof REPO>();
    const value = app({
      createCodeRepo: vi.fn(() => registration.promise),
    });
    render(<PaletteHarness value={value} />);
    fireEvent.click(
      await screen.findByRole("option", { name: /Local folder/ }),
    );
    fireEvent.change(await screen.findByLabelText("Path"), {
      target: { value: REPO.root_path },
    });
    fireEvent.click(screen.getByRole("button", { name: "Register" }));
    fireEvent.click(screen.getByRole("button", { name: "Close" }));

    await act(async () => {
      registration.resolve(REPO);
      await registration.promise;
    });

    expect(useCodeCatalogStore.getState().repos).toContainEqual(REPO);
    expect(toast.success).toHaveBeenCalledWith(
      "Repository registered",
      expect.objectContaining({
        description: "Create a workspace when you are ready.",
      }),
    );
    expect(useCodeUiStore.getState().newWorkspaceOpen).toBe(false);
    expect(useCodeUiStore.getState().newWorkspaceRepoId).toBeUndefined();
    const options = vi.mocked(toast.success).mock.calls[0]?.[1] as
      | { action?: { onClick: () => void } }
      | undefined;
    options?.action?.onClick();
    expect(useCodeUiStore.getState().newWorkspaceRepoId).toBe(REPO.id);
  });

  it("ignores slow setup probes from a dismissed opening after reopen", async () => {
    const oldSources =
      deferred<
        Awaited<ReturnType<AppContextValue["client"]["getCodeRepoSources"]>>
      >();
    const oldGithub =
      deferred<
        Awaited<
          ReturnType<AppContextValue["client"]["listCodeGithubRepositories"]>
        >
      >();
    const oldDefaults =
      deferred<
        Awaited<ReturnType<AppContextValue["client"]["getCodeCloneDefaults"]>>
      >();
    const getCodeRepoSources = vi
      .fn()
      .mockImplementationOnce(() => oldSources.promise)
      .mockResolvedValue({
        sources: [
          {
            kind: "local",
            available: false,
            remediation: "Local folders are unavailable on this machine.",
          },
          { kind: "git_url", available: true },
          { kind: "github", available: true },
        ],
        chooses_destination: false,
      });
    const listCodeGithubRepositories = vi
      .fn()
      .mockImplementationOnce(() => oldGithub.promise)
      .mockResolvedValue({
        repositories: [{ full_name: "new-owner/new-repo", private: true }],
      });
    const getCodeCloneDefaults = vi
      .fn()
      .mockImplementationOnce(() => oldDefaults.promise)
      .mockResolvedValue({
        parent_dir: "/new/src",
        gh_found: true,
        gh_authenticated: true,
      });
    render(
      <PaletteHarness
        value={app({
          getCodeRepoSources,
          listCodeGithubRepositories,
          getCodeCloneDefaults,
        })}
      />,
    );

    await waitFor(() => {
      expect(getCodeRepoSources).toHaveBeenCalledTimes(1);
      expect(listCodeGithubRepositories).toHaveBeenCalledTimes(1);
      expect(getCodeCloneDefaults).toHaveBeenCalledTimes(1);
    });
    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    fireEvent.click(
      screen.getByRole("button", { name: "Open add repository" }),
    );
    await waitFor(() => {
      expect(getCodeRepoSources).toHaveBeenCalledTimes(2);
      expect(listCodeGithubRepositories).toHaveBeenCalledTimes(2);
      expect(getCodeCloneDefaults).toHaveBeenCalledTimes(2);
    });
    await waitFor(() =>
      expect(
        screen.queryByRole("option", { name: /Local folder/ }),
      ).not.toBeInTheDocument(),
    );

    await act(async () => {
      oldSources.resolve({
        sources: [
          { kind: "local", available: true },
          { kind: "git_url", available: true },
          { kind: "github", available: true },
        ],
        chooses_destination: false,
      });
      oldGithub.resolve({
        repositories: [{ full_name: "old-owner/old-repo", private: true }],
      });
      oldDefaults.resolve({
        parent_dir: "/old/src",
        gh_found: true,
        gh_authenticated: true,
        gh_remediation: "",
      });
      await Promise.all([
        oldSources.promise,
        oldGithub.promise,
        oldDefaults.promise,
      ]);
    });

    expect(
      screen.queryByRole("option", { name: /Local folder/ }),
    ).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("option", { name: /GitHub repository/ }));
    expect(await screen.findByLabelText("Destination folder")).toHaveValue(
      "/new/src",
    );
    const user = userEvent.setup();
    await user.click(screen.getByRole("combobox", { name: "Repository" }));
    expect(
      await screen.findByRole("option", { name: /new-owner\/new-repo/ }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("option", { name: /old-owner\/old-repo/ }),
    ).not.toBeInTheDocument();
  });

  it("ignores setup probes from a replaced ApiClient", async () => {
    const oldSources =
      deferred<
        Awaited<ReturnType<AppContextValue["client"]["getCodeRepoSources"]>>
      >();
    const oldGithub =
      deferred<
        Awaited<
          ReturnType<AppContextValue["client"]["listCodeGithubRepositories"]>
        >
      >();
    const oldDefaults =
      deferred<
        Awaited<ReturnType<AppContextValue["client"]["getCodeCloneDefaults"]>>
      >();
    const oldValue = app({
      getCodeRepoSources: vi.fn(() => oldSources.promise),
      listCodeGithubRepositories: vi.fn(() => oldGithub.promise),
      getCodeCloneDefaults: vi.fn(() => oldDefaults.promise),
    });
    const replacement = app({
      getCodeRepoSources: vi.fn(async () => ({
        sources: [
          { kind: "local", available: true },
          { kind: "git_url", available: true },
          { kind: "github", available: true },
        ],
        chooses_destination: false,
      })),
      listCodeGithubRepositories: vi.fn(async () => ({
        repositories: [{ full_name: "replacement/repository", private: true }],
      })),
      getCodeCloneDefaults: vi.fn(async () => ({
        parent_dir: "/replacement/src",
        gh_found: true,
        gh_authenticated: true,
        gh_remediation: "",
      })),
    });
    const view = render(<PaletteHarness value={oldValue} />);
    await waitFor(() =>
      expect(oldValue.client.getCodeRepoSources).toHaveBeenCalled(),
    );

    view.rerender(<PaletteHarness value={replacement} />);
    fireEvent.click(
      await screen.findByRole("option", { name: /GitHub repository/ }),
    );
    expect(await screen.findByLabelText("Destination folder")).toHaveValue(
      "/replacement/src",
    );

    await act(async () => {
      oldSources.resolve({
        sources: [
          { kind: "local", available: false },
          { kind: "git_url", available: false },
          { kind: "github", available: true },
        ],
        chooses_destination: true,
      });
      oldGithub.resolve({
        repositories: [{ full_name: "stale/repository", private: true }],
      });
      oldDefaults.resolve({
        parent_dir: "/stale/src",
        gh_found: false,
        gh_authenticated: false,
        gh_remediation: "stale credential result",
      });
      await Promise.all([
        oldSources.promise,
        oldGithub.promise,
        oldDefaults.promise,
      ]);
    });

    expect(screen.getByLabelText("Destination folder")).toHaveValue(
      "/replacement/src",
    );
    expect(
      screen.queryByText("stale credential result"),
    ).not.toBeInTheDocument();
    const user = userEvent.setup();
    await user.click(screen.getByRole("combobox", { name: "Repository" }));
    expect(
      await screen.findByRole("option", { name: /replacement\/repository/ }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("option", { name: /stale\/repository/ }),
    ).not.toBeInTheDocument();
  });

  it("clears the previous client's defaults while replacement probes wait", async () => {
    const replacementDefaults =
      deferred<
        Awaited<ReturnType<AppContextValue["client"]["getCodeCloneDefaults"]>>
      >();
    const first = app({
      getCodeCloneDefaults: vi.fn(async () => ({
        parent_dir: "/first/src",
        gh_found: false,
        gh_authenticated: false,
        gh_remediation: "Client A needs a GitHub credential.",
      })),
    });
    const replacement = app({
      getCodeCloneDefaults: vi.fn(() => replacementDefaults.promise),
    });
    const view = render(<PaletteHarness value={first} />);
    fireEvent.click(
      await screen.findByRole("option", { name: /GitHub repository/ }),
    );
    expect(await screen.findByTestId("gh-absent-hint")).toHaveTextContent(
      "Client A needs a GitHub credential.",
    );

    view.rerender(<PaletteHarness value={replacement} />);
    fireEvent.click(
      await screen.findByRole("option", { name: /GitHub repository/ }),
    );

    expect(
      screen.queryByText(/Client A needs a GitHub credential/),
    ).not.toBeInTheDocument();
  });

  it.each([
    {
      name: "completed",
      job: {
        id: "job-terminal",
        phase: "Clone complete",
        done: true,
        repo_id: REPO.id,
      },
    },
    {
      name: "failed",
      job: {
        id: "job-terminal",
        phase: "Clone failed",
        done: true,
        error: "fatal: repository not found",
      },
    },
  ])("forgets a $name clone when Back leaves progress", async ({ job }) => {
    const value = app();
    activateCodeCloneClient(value.client);
    trackCodeClone(value.client, job, {
      url: "https://example.com/acme/app.git",
      parent_dir: "/tmp/src",
    });
    render(<PaletteHarness value={value} />);
    expect(await screen.findByTestId("clone-phase")).toHaveTextContent(
      job.phase,
    );

    fireEvent.keyDown(screen.getByRole("dialog"), { key: "Backspace" });
    expect(
      await screen.findByPlaceholderText("https://example.com/acme/app.git"),
    ).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    fireEvent.click(
      screen.getByRole("button", { name: "Open add repository" }),
    );

    expect(
      await screen.findByRole("option", { name: /Local folder/ }),
    ).toBeVisible();
    expect(
      useCodeUpdatesStore.getState().cloneJobs["job-terminal"],
    ).toBeUndefined();
  });

  it("recovers a clone that completes while Code UI is unmounted", async () => {
    let completed = false;
    const getCodeCloneJob = vi.fn(async () =>
      completed
        ? {
            id: "job-unmounted",
            phase: "complete",
            done: true,
            repo_id: REPO.id,
          }
        : {
            id: "job-unmounted",
            phase: "receiving objects",
            percent: 48,
            done: false,
          },
    );
    const getCodeRepo = vi.fn(async () => REPO);
    const value = app({
      startCodeClone: vi.fn(async () => ({
        id: "job-unmounted",
        phase: "starting",
        done: false,
      })),
      getCodeCloneJob,
      getCodeRepo,
    });
    const view = render(<PaletteHarness value={value} />);
    await startGitClone();
    await waitFor(() => expect(getCodeCloneJob).toHaveBeenCalled());

    completed = true;
    view.rerender(<PaletteHarness value={value} mounted={false} />);
    act(() => useCodeUpdatesStore.getState().resetLive());
    view.rerender(<PaletteHarness value={value} />);

    expect(await screen.findByText("The repository is ready.")).toBeVisible();
    expect(getCodeRepo).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Create workspace" }));
    await waitFor(() => expect(getCodeRepo).toHaveBeenCalledWith(REPO.id));
    expect(useCodeUiStore.getState().newWorkspaceRepoId).toBe(REPO.id);
  });

  it("recovers a missed completion notice from durable clone state", async () => {
    const getCodeRepo = vi.fn(async () => REPO);
    const value = app({
      startCodeClone: vi.fn(async () => ({
        id: "job-missed-notice",
        phase: "starting",
        done: false,
      })),
      getCodeCloneJob: vi.fn(async () => ({
        id: "job-missed-notice",
        phase: "complete",
        done: true,
        repo_id: REPO.id,
      })),
      getCodeRepo,
    });
    render(<PaletteHarness value={value} />);

    await startGitClone();
    await waitFor(() => expect(getCodeRepo).toHaveBeenCalledWith(REPO.id));
    expect(useCodeUiStore.getState().newWorkspaceOpen).toBe(true);
    expect(useCodeUiStore.getState().newWorkspaceRepoId).toBe(REPO.id);
  });

  it("retries a failed durable progress read", async () => {
    const getCodeCloneJob = vi
      .fn()
      .mockRejectedValueOnce(new Error("The progress service is unavailable."))
      .mockResolvedValue({
        id: "job-retry-read",
        phase: "complete",
        done: true,
        repo_id: REPO.id,
      });
    const getCodeRepo = vi.fn(async () => REPO);
    const value = app({
      startCodeClone: vi.fn(async () => ({
        id: "job-retry-read",
        phase: "starting",
        done: false,
      })),
      getCodeCloneJob,
      getCodeRepo,
    });
    render(<PaletteHarness value={value} />);

    await startGitClone();
    expect(
      await screen.findByText("The progress service is unavailable."),
    ).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: "Retry check" }));
    await waitFor(() => expect(getCodeCloneJob).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(getCodeRepo).toHaveBeenCalledWith(REPO.id));
  });

  it("resumes a dismissed clone without rearming its automatic handoff", async () => {
    const getCodeRepo = vi.fn(async () => REPO);
    const value = app({
      startCodeClone: vi.fn(async () => ({
        id: "job-resume",
        phase: "receiving objects",
        percent: 32,
        done: false,
      })),
      getCodeCloneJob: vi.fn(async () => ({
        id: "job-resume",
        phase: "receiving objects",
        percent: 32,
        done: false,
      })),
      getCodeRepo,
    });
    render(<PaletteHarness value={value} />);

    await startGitClone();
    expect(await screen.findByTestId("clone-phase")).toHaveTextContent(
      "receiving objects",
    );
    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    fireEvent.click(
      screen.getByRole("button", { name: "Open add repository" }),
    );
    expect(await screen.findByTestId("clone-phase")).toHaveTextContent(
      "receiving objects",
    );

    act(() => {
      useCodeUpdatesStore.getState().apply({
        type: "clone_progress",
        job: {
          id: "job-resume",
          phase: "complete",
          done: true,
          repo_id: REPO.id,
        },
      });
    });

    expect(await screen.findByText("The repository is ready.")).toBeVisible();
    expect(getCodeRepo).not.toHaveBeenCalled();
    expect(
      screen.getByRole("button", { name: "Create workspace" }),
    ).toBeVisible();
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
    const user = userEvent.setup();
    fireEvent.click(
      await screen.findByRole("option", { name: /GitHub repository/ }),
    );
    await user.click(
      await screen.findByRole("combobox", { name: "Repository" }),
    );
    expect(
      await screen.findByRole("option", { name: /mira-chen\/notes/ }),
    ).toBeInTheDocument();
    await user.click(screen.getByRole("option", { name: /mira-chen\/notes/ }));
    expect(
      screen.getByRole("combobox", { name: "Repository" }),
    ).toHaveTextContent("mira-chen/notes");
  });

  it("keeps the picker when the list fails so a name can still be typed", async () => {
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
        listCodeGithubRepositories: vi.fn(async () => {
          throw new Error("502: bad gateway");
        }),
        getCodeCloneDefaults: vi.fn(async () => {
          throw new Error("403: forbidden");
        }),
      } as never),
    );
    const user = userEvent.setup();
    fireEvent.click(
      await screen.findByRole("option", { name: /GitHub repository/ }),
    );
    expect(await screen.findByTestId("github-list-failed")).toHaveTextContent(
      /Suggestions did not load/,
    );
    const picker = await screen.findByRole("combobox", { name: "Repository" });
    await user.click(picker);
    await user.type(
      screen.getByPlaceholderText("Search or type owner/repo"),
      "acme/app",
    );
    await user.click(screen.getByRole("option", { name: "acme/app" }));
    expect(picker).toHaveTextContent("acme/app");
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
    const user = userEvent.setup();
    fireEvent.click(
      await screen.findByRole("option", { name: /GitHub repository/ }),
    );
    await user.click(
      await screen.findByRole("combobox", { name: "Repository" }),
    );
    await user.click(
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
