import { useEffect, useState } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import { expect, userEvent, within } from "storybook/test";

import { AppContextProvider, type AppContextValue } from "@/AppContext";
import type { ApiClient } from "@/api/client";
import type { HarnessKind } from "@/api/types";
import { Toaster } from "@/components/ui/sonner";
import { AddRepoPalette } from "@/code/AddRepoPalette";
import { useCodeCatalogStore } from "@/code/CodeCatalogStore";
import { CodeRepoEmptyState } from "@/code/CodeHome";
import { useCodeUiStore } from "@/code/CodeUiStore";
import {
  activateCodeCloneClient,
  trackCodeClone,
  useCodeUpdatesStore,
} from "@/code/CodeUpdatesStore";
import { NewWorkspaceDialog } from "@/code/NewWorkspaceDialog";
import {
  codeRepositories,
  harnessDoctor,
  harnessDoctorDegraded,
} from "./fixtures";

type SetupScenario =
  | "add"
  | "github-hint"
  | "local-only"
  | "clone-failure"
  | "clone-progress"
  | "clone-background-complete"
  | "clone-complete"
  | "hosted-picker"
  | "hosted-list-failed"
  | "source-probe-failure"
  | "defaults-probe-failure"
  | "progress-read-failure"
  | "repository-handoff-failure"
  | "registration-background-complete"
  | "workspace"
  | "workspace-needs-harness";

function pending<T>(): Promise<T> {
  return new Promise(() => {});
}

function setupClient(scenario: SetupScenario): ApiClient {
  const localOnly = scenario === "local-only";
  const githubHint = scenario === "github-hint";
  return {
    getCodeCloneDefaults: async () => {
      if (scenario === "defaults-probe-failure") {
        throw new Error("The saved destination is unavailable.");
      }
      return {
        parent_dir: "/Users/sam/src",
        gh_found: !githubHint,
        gh_authenticated: !githubHint,
        gh_remediation: githubHint
          ? "GitHub CLI is not signed in on this machine."
          : "",
      };
    },
    listCodeGithubRepositories: async () => {
      if (scenario === "hosted-list-failed") {
        throw new Error("502: bad gateway");
      }
      return scenario === "hosted-picker"
        ? {
            repositories: [
              {
                full_name: "brightwave-inc/tidebreak",
                private: true,
                description:
                  "Open-source, local-first desktop for AI coding agents and documents",
              },
              {
                full_name: "mira-chen/notes",
                private: true,
                description: "scratch",
              },
              {
                full_name: "brightwave-inc/shipright",
                private: true,
                description: "Review bots for pull requests",
              },
              {
                full_name: "mira-chen/dotfiles",
                private: true,
                description: "machine setup",
              },
              {
                full_name: "brightwave-inc/model-gateway",
                private: true,
                description: "Self-hosted LLM and MCP aggregation gateway",
              },
              {
                full_name: "brightwave-inc/clawdbot",
                private: true,
                description: "",
              },
              {
                full_name: "brightwave-inc/terraform-main",
                private: true,
                description: "All things terraform related",
              },
              {
                full_name: "brightwave-inc/tidebreak-site",
                private: true,
                description: "Marketing site",
              },
              {
                full_name: "brightwave-inc/orca",
                private: true,
                description: "Internal agent workspace tooling",
              },
            ],
          }
        : { repositories: [] };
    },
    getCodeRepoSources: async () => {
      if (scenario === "source-probe-failure") {
        throw new Error("The machine did not answer.");
      }
      return {
        sources: localOnly
          ? [
              { kind: "local", available: true },
              {
                kind: "git_url",
                available: false,
                remediation: "Install git on this machine to clone a remote.",
              },
              {
                kind: "github",
                available: false,
                remediation: "Install git on this machine to clone a remote.",
              },
            ]
          : [
              { kind: "local", available: true },
              { kind: "git_url", available: true },
              {
                kind: "github",
                available: true,
                remediation:
                  scenario === "hosted-picker" ||
                  scenario === "hosted-list-failed"
                    ? "Clones and pushes use your own GitHub account: work lands as mira-chen."
                    : githubHint
                      ? "GitHub CLI is not signed in on this machine."
                      : undefined,
              },
            ],
        chooses_destination:
          scenario === "hosted-picker" || scenario === "hosted-list-failed",
      };
    },
    startCodeClone: async () => {
      return {
        id: "clone-story",
        phase: "Receiving objects",
        percent: 38,
        done: false,
      };
    },
    getCodeCloneJob: async (jobId: string) => {
      if (scenario === "progress-read-failure") {
        throw new Error("The progress service is unavailable.");
      }
      return (
        useCodeUpdatesStore.getState().cloneJobs[jobId] ?? {
          id: jobId,
          phase: "Receiving objects",
          percent: 38,
          done: false,
        }
      );
    },
    getCodeRepo: async () => {
      if (scenario === "repository-handoff-failure") {
        throw new Error("The repository catalog is unavailable.");
      }
      return codeRepositories[0]!;
    },
    createCodeRepo: async () => {
      if (scenario === "registration-background-complete") {
        await new Promise((resolve) => globalThis.setTimeout(resolve, 250));
      }
      return codeRepositories[0]!;
    },
    listCodeHarnessModels: async (kind: HarnessKind) => ({
      kind,
      models: [
        {
          id: kind === "claude_code" ? "claude-sonnet-5" : "gpt-5.6-sol",
          label: kind === "claude_code" ? "Claude Sonnet 5" : "GPT 5.6 Sol",
          default: true,
          reasoning_efforts: [],
          fast_mode: false,
        },
      ],
      reasoning_efforts: [],
    }),
    createCodeWorkspace: async (
      body: Parameters<ApiClient["createCodeWorkspace"]>[0],
    ) => ({
      id: "ws-new-story",
      repo_id: body.repo_id,
      title: body.title ?? "Focused workspace",
      worktree_path: "/Users/sam/tidebreak/worktrees/focused-workspace",
      branch_name: "thet/focused-workspace",
      base_ref: body.base_ref ?? "main",
      status: "active",
      created_at: "2026-08-24T14:10:00.000Z",
    }),
    createCodeSession: async (
      workspaceId: string,
      body: Parameters<ApiClient["createCodeSession"]>[1],
    ) => ({
      id: "session-new-story",
      workspace_id: workspaceId,
      kind: "interactive",
      harness_kind: body.harness,
      permission_mode: body.permission_mode,
      lifecycle: "idle",
      attention: { state: { type: "working" }, source: "lifecycle" },
      unrecognized_event_count: 0,
      created_at: "2026-08-24T14:10:10.000Z",
    }),
    startHarnessInstall: async () => pending(),
    getHarnessDoctor: async () =>
      scenario === "workspace-needs-harness"
        ? harnessDoctorDegraded
        : harnessDoctor,
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

function setupRouter() {
  const rootRoute = createRootRoute();
  const codeRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/code",
    component: () => <CodeRepoEmptyState onAddRepo={() => {}} />,
  });
  const workspaceRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/code/w/$workspaceId",
    component: () => <p>Workspace created</p>,
  });
  return createRouter({
    routeTree: rootRoute.addChildren([codeRoute, workspaceRoute]),
    history: createMemoryHistory({ initialEntries: ["/code"] }),
  });
}

const CLONE_REQUEST = {
  url: "https://github.com/brightwave-inc/tidebreak.git",
  parent_dir: "/Users/sam/src",
};

function seedCloneScenario(client: ApiClient, scenario: SetupScenario) {
  activateCodeCloneClient(client);
  if (scenario === "clone-progress" || scenario === "progress-read-failure") {
    trackCodeClone(
      client,
      {
        id: "clone-story",
        phase: "Receiving objects",
        percent: 38,
        done: false,
      },
      CLONE_REQUEST,
    );
  }
  if (scenario === "clone-background-complete") {
    trackCodeClone(
      client,
      {
        id: "clone-story",
        phase: "Receiving objects",
        percent: 82,
        done: false,
      },
      CLONE_REQUEST,
      true,
    );
  }
  if (scenario === "clone-failure") {
    trackCodeClone(
      client,
      {
        id: "clone-story",
        phase: "Clone failed",
        done: true,
        error: "fatal: repository not found",
      },
      CLONE_REQUEST,
    );
  }
  if (
    scenario === "clone-complete" ||
    scenario === "repository-handoff-failure"
  ) {
    trackCodeClone(
      client,
      {
        id: "clone-story",
        phase: "Clone complete",
        done: true,
        repo_id: codeRepositories[0]!.id,
      },
      CLONE_REQUEST,
    );
  }
}

function RepositorySetupStory({ scenario }: { scenario: SetupScenario }) {
  const [paletteOpen, setPaletteOpen] = useState(
    scenario !== "clone-background-complete",
  );
  const [state] = useState(() => {
    useCodeCatalogStore.getState().reset();
    useCodeUpdatesStore.getState().reset();
    useCodeUiStore.setState({
      addRepoOpen: false,
      lastCreate: null,
      newWorkspaceOpen: false,
      newWorkspaceRepoId: undefined,
      pendingComposerPrompt: null,
    });
    useCodeCatalogStore.setState({
      repos: codeRepositories,
      workspaces: [],
      doctor:
        scenario === "workspace-needs-harness"
          ? harnessDoctorDegraded
          : harnessDoctor,
      loaded: true,
    });
    const client = setupClient(scenario);
    seedCloneScenario(client, scenario);
    return { client, router: setupRouter() };
  });

  useEffect(() => {
    if (scenario === "clone-background-complete") {
      useCodeUpdatesStore.getState().apply({
        type: "clone_progress",
        job: {
          id: "clone-story",
          phase: "Clone complete",
          done: true,
          repo_id: codeRepositories[0]!.id,
        },
      });
    }
    return () => {
      useCodeCatalogStore.getState().reset();
      useCodeUpdatesStore.getState().reset();
    };
  }, [scenario]);

  const workspace = scenario.startsWith("workspace");
  return (
    <AppContextProvider value={appContext(state.client)}>
      <RouterProvider router={state.router as never} />
      {workspace ? (
        <NewWorkspaceDialog
          open
          onOpenChange={() => {}}
          repos={codeRepositories}
        />
      ) : (
        <AddRepoPalette open={paletteOpen} onOpenChange={setPaletteOpen} />
      )}
      <Toaster richColors duration={Number.POSITIVE_INFINITY} />
    </AppContextProvider>
  );
}

const meta = {
  title: "Code/Repository setup",
  component: RepositorySetupStory,
  args: { scenario: "add" },
  parameters: { layout: "fullscreen" },
  render: (args) => <RepositorySetupStory key={args.scenario} {...args} />,
} satisfies Meta<typeof RepositorySetupStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const AddRepository: Story = {};

export const GitHubWithoutCredential: Story = {
  args: { scenario: "github-hint" },
  play: async ({ canvasElement }) => {
    const body = within(canvasElement.ownerDocument.body);
    await userEvent.click(
      await body.findByRole("option", { name: /GitHub repository/ }),
    );
    await expect(await body.findByTestId("gh-absent-hint")).toBeVisible();
  },
};

export const LocalOnlyMachine: Story = {
  args: { scenario: "local-only" },
  play: async ({ canvasElement }) => {
    const body = within(canvasElement.ownerDocument.body);
    await expect(
      await body.findByText("Install git on this machine to clone a remote."),
    ).toBeVisible();
  },
};

export const CloneFailure: Story = {
  args: { scenario: "clone-failure" },
  play: async ({ canvasElement }) => {
    const body = within(canvasElement.ownerDocument.body);
    await expect(
      await body.findByText("fatal: repository not found"),
    ).toBeVisible();
    await expect(
      await body.findByRole("button", { name: "Retry" }),
    ).toBeVisible();
  },
};

export const HostedGitHubPicker: Story = {
  args: { scenario: "hosted-picker" },
  play: async ({ canvasElement }) => {
    const body = within(canvasElement.ownerDocument.body);
    await userEvent.click(
      await body.findByRole("option", { name: /GitHub repository/ }),
    );
    await userEvent.click(
      await body.findByRole("combobox", { name: "Repository" }),
    );
    await expect(
      await body.findByRole("option", { name: /mira-chen\/notes/ }),
    ).toBeVisible();
    const last = await body.findByRole("option", {
      name: /mira-chen\/notes/,
    });
    last.scrollIntoView();
    await expect(last).toBeVisible();
    await expect(body.queryByText("Destination folder")).toBeNull();
  },
};

export const HostedGitHubListFailed: Story = {
  args: { scenario: "hosted-list-failed" },
  play: async ({ canvasElement }) => {
    const body = within(canvasElement.ownerDocument.body);
    await userEvent.click(
      await body.findByRole("option", { name: /GitHub repository/ }),
    );
    await expect(
      await body.findByRole("combobox", { name: "Repository" }),
    ).toBeVisible();
    await expect(await body.findByTestId("github-list-failed")).toBeVisible();
  },
};

export const SourceProbeFailure: Story = {
  args: { scenario: "source-probe-failure" },
  play: async ({ canvasElement }) => {
    const body = within(canvasElement.ownerDocument.body);
    await expect(
      await body.findByText(
        "Could not check which sources this machine supports.",
      ),
    ).toBeVisible();
    await userEvent.click(await body.findByRole("button", { name: "Retry" }));
    await expect(
      await body.findByText(
        "Could not check which sources this machine supports.",
      ),
    ).toBeVisible();
  },
};

export const DefaultsProbeFailure: Story = {
  args: { scenario: "defaults-probe-failure" },
  play: async ({ canvasElement }) => {
    const body = within(canvasElement.ownerDocument.body);
    await userEvent.click(await body.findByRole("option", { name: /Git URL/ }));
    await expect(
      await body.findByText(
        "The saved destination did not load. Choose one or retry.",
      ),
    ).toBeVisible();
    await userEvent.click(await body.findByRole("button", { name: "Retry" }));
    await expect(
      await body.findByText(
        "The saved destination did not load. Choose one or retry.",
      ),
    ).toBeVisible();
  },
};

export const CloneProgress: Story = {
  args: { scenario: "clone-progress" },
  play: async ({ canvasElement }) => {
    const body = within(canvasElement.ownerDocument.body);
    await expect(await body.findByText("Receiving objects")).toBeVisible();
  },
};

export const DurableProgressReadFailure: Story = {
  args: { scenario: "progress-read-failure" },
  play: async ({ canvasElement }) => {
    const body = within(canvasElement.ownerDocument.body);
    await expect(
      await body.findByText("The progress service is unavailable."),
    ).toBeVisible();
    await userEvent.click(
      await body.findByRole("button", { name: "Retry check" }),
    );
    await expect(
      await body.findByText("The progress service is unavailable."),
    ).toBeVisible();
  },
};

export const BackgroundCloneCompleted: Story = {
  args: { scenario: "clone-background-complete" },
  play: async ({ canvasElement }) => {
    const body = within(canvasElement.ownerDocument.body);
    await expect(await body.findByText("Repository cloned")).toBeVisible();
    await expect(
      await body.findByText("Create a workspace when you are ready."),
    ).toBeVisible();
  },
};

export const CloneCompleted: Story = {
  args: { scenario: "clone-complete" },
  play: async ({ canvasElement }) => {
    const body = within(canvasElement.ownerDocument.body);
    await expect(
      await body.findByText("The repository is ready."),
    ).toBeVisible();
    await expect(
      await body.findByRole("button", { name: "Create workspace" }),
    ).toBeVisible();
  },
};

export const RepositoryHandoffFailure: Story = {
  args: { scenario: "repository-handoff-failure" },
  play: async ({ canvasElement }) => {
    const body = within(canvasElement.ownerDocument.body);
    await userEvent.click(
      await body.findByRole("button", { name: "Create workspace" }),
    );
    await expect(
      await body.findByText("The repository catalog is unavailable."),
    ).toBeVisible();
    await userEvent.click(await body.findByRole("button", { name: "Retry" }));
    await expect(
      await body.findByText("The repository catalog is unavailable."),
    ).toBeVisible();
  },
};

export const RegistrationCompletedInBackground: Story = {
  args: { scenario: "registration-background-complete" },
  play: async ({ canvasElement }) => {
    const body = within(canvasElement.ownerDocument.body);
    await userEvent.click(
      await body.findByRole("option", { name: /Local folder/ }),
    );
    await userEvent.type(await body.findByLabelText("Path"), "/Users/sam/src");
    await userEvent.click(
      await body.findByRole("button", { name: "Register" }),
    );
    await userEvent.click(await body.findByRole("button", { name: "Close" }));
    await expect(await body.findByText("Repository registered")).toBeVisible();
    await expect(
      await body.findByRole("button", { name: "Open" }),
    ).toBeVisible();
  },
};

export const NewWorkspace: Story = { args: { scenario: "workspace" } };

export const NewWorkspaceNeedsHarness: Story = {
  args: { scenario: "workspace-needs-harness" },
};

export const CompactNewWorkspace: Story = {
  args: { scenario: "workspace" },
  globals: { viewport: { value: "compact", isRotated: false } },
};
