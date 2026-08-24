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
import { AddRepoPalette } from "@/code/AddRepoPalette";
import { useCodeCatalogStore } from "@/code/CodeCatalogStore";
import { useCodeUiStore } from "@/code/CodeUiStore";
import { useCodeUpdatesStore } from "@/code/CodeUpdatesStore";
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
  | "workspace"
  | "workspace-needs-harness";

function pending<T>(): Promise<T> {
  return new Promise(() => {});
}

function setupClient(scenario: SetupScenario): ApiClient {
  const localOnly = scenario === "local-only";
  const githubHint = scenario === "github-hint";
  return {
    getCodeCloneDefaults: async () => ({
      parent_dir: "/Users/sam/src",
      gh_found: !githubHint,
      gh_authenticated: !githubHint,
      gh_remediation: githubHint
        ? "GitHub CLI is not signed in on this machine."
        : "",
    }),
    getCodeRepoSources: async () => ({
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
              remediation: githubHint
                ? "GitHub CLI is not signed in on this machine."
                : undefined,
            },
          ],
      chooses_destination: false,
    }),
    startCodeClone: async () => {
      if (scenario === "clone-failure") {
        throw new Error("The remote repository could not be reached.");
      }
      return {
        id: "clone-story",
        phase: "Receiving objects",
        percent: 38,
        done: false,
      };
    },
    getCodeRepo: async () => codeRepositories[0]!,
    createCodeRepo: async () => codeRepositories[0]!,
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
    component: () => <p>Code</p>,
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

function RepositorySetupStory({ scenario }: { scenario: SetupScenario }) {
  const [state] = useState(() => {
    useCodeCatalogStore.getState().reset();
    useCodeUpdatesStore.getState().reset();
    useCodeUiStore.setState({ lastCreate: null, pendingComposerPrompt: null });
    useCodeCatalogStore.setState({
      repos: codeRepositories,
      workspaces: [],
      doctor:
        scenario === "workspace-needs-harness"
          ? harnessDoctorDegraded
          : harnessDoctor,
      loaded: true,
    });
    return { client: setupClient(scenario), router: setupRouter() };
  });

  useEffect(
    () => () => {
      useCodeCatalogStore.getState().reset();
      useCodeUpdatesStore.getState().reset();
    },
    [],
  );

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
        <AddRepoPalette open onOpenChange={() => {}} />
      )}
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
    await userEvent.click(await body.findByRole("option", { name: /Git URL/ }));
    await userEvent.type(
      await body.findByPlaceholderText("https://example.com/acme/app.git"),
      "https://github.com/brightwave-inc/missing.git",
    );
    await userEvent.click(await body.findByRole("button", { name: "Clone" }));
    await expect(
      await body.findByText("The remote repository could not be reached."),
    ).toBeVisible();
  },
};

export const CloneProgress: Story = {
  args: { scenario: "clone-progress" },
  play: async ({ canvasElement }) => {
    const body = within(canvasElement.ownerDocument.body);
    await userEvent.click(await body.findByRole("option", { name: /Git URL/ }));
    await userEvent.type(
      await body.findByPlaceholderText("https://example.com/acme/app.git"),
      "https://github.com/brightwave-inc/tidebreak.git",
    );
    await userEvent.click(await body.findByRole("button", { name: "Clone" }));
    await expect(await body.findByText("Receiving objects")).toBeVisible();
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
