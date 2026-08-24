import { useEffect, useState, type ReactNode } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";

import { AppContextProvider, type AppContextValue } from "@/AppContext";
import type { CodeRepoSnapshot, HarnessKind } from "@/api/types";
import { useCodeCatalogStore } from "@/code/CodeCatalogStore";
import { NewWorkspaceDialog } from "@/code/NewWorkspaceDialog";
import { harnessDoctor, harnessDoctorDegraded } from "./fixtures";

/**
 * The new-workspace composer: the first message is the surface, and every
 * setting — repo, name, base ref, engine, model, permissions — is a pill with
 * its own chord. Enter creates; "Create more" keeps it open for the next one.
 */

const repos: CodeRepoSnapshot[] = [
  {
    id: "repo-tidebreak",
    root_path: "/Users/sam/tidebreak",
    display_name: "tidebreak",
    default_base_ref: "main",
    branch_prefix: "sam/",
    quick_actions: [],
    created_at: "2026-08-10T12:00:00.000Z",
  },
  {
    id: "repo-toronto",
    root_path: "/Users/sam/toronto",
    display_name: "toronto",
    default_base_ref: "main",
    branch_prefix: "sam/",
    quick_actions: [],
    created_at: "2026-08-12T12:00:00.000Z",
  },
  {
    id: "repo-georgetown",
    root_path: "/Users/sam/georgetown",
    display_name: "georgetown",
    default_base_ref: "develop",
    branch_prefix: "sam/",
    quick_actions: [],
    created_at: "2026-08-14T12:00:00.000Z",
  },
];

const MODELS: Partial<
  Record<HarnessKind, { id: string; label: string; default?: boolean }[]>
> = {
  claude_code: [
    { id: "claude-opus-5", label: "Claude Opus 5", default: true },
    { id: "claude-sonnet-5", label: "Claude Sonnet 5" },
  ],
  codex: [
    { id: "gpt-5.6-sol", label: "GPT 5.6 Sol", default: true },
    { id: "gpt-5.6-luna", label: "GPT 5.6 Luna" },
  ],
  opencode: [
    { id: "gpt-5.6-sol", label: "GPT 5.6 Sol", default: true },
    { id: "claude-opus-5", label: "Claude Opus 5" },
    { id: "grok-4.5", label: "Grok 4.5" },
  ],
};

function appContext(): AppContextValue {
  const client = {
    listCodeHarnessModels: async (kind: HarnessKind) => ({
      kind,
      models: MODELS[kind] ?? [],
    }),
    startHarnessInstall: async (kind: HarnessKind) => ({
      kind,
      version: "2.1.240",
      phase: "download",
      done: false,
    }),
    getHarnessDoctor: async () => harnessDoctor,
    // The story has no server: create reports itself as stubbed rather than
    // handing the dialog an empty snapshot.
    createCodeWorkspace: async () => {
      throw new Error("Storybook stub: creates are not wired here.");
    },
    createCodeSession: fn(),
    submitCodeTurn: fn(),
  };
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

function withRouter(children: ReactNode) {
  const rootRoute = createRootRoute();
  const index = createRoute({
    getParentRoute: () => rootRoute,
    path: "/",
    component: () => <>{children}</>,
  });
  const workspaceRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/code/w/$workspaceId",
    component: () => <>{children}</>,
  });
  const harnessSettingsRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/settings/coding-harnesses",
    component: () => <>{children}</>,
  });
  const router = createRouter({
    routeTree: rootRoute.addChildren([
      index,
      workspaceRoute,
      harnessSettingsRoute,
    ]),
    history: createMemoryHistory({ initialEntries: ["/"] }),
  });
  return <RouterProvider router={router as never} />;
}

function NewWorkspace({
  doctor = harnessDoctor,
}: {
  doctor?: typeof harnessDoctor;
}) {
  // Seed the catalog before the dialog mounts: its defaults read the store.
  const [seeded, setSeeded] = useState(false);
  useEffect(() => {
    useCodeCatalogStore.setState({
      repos,
      doctor,
      sessionsByWorkspace: {},
      workspaces: [],
    });
    setSeeded(true);
  }, [doctor]);
  if (!seeded) return null;
  return (
    <AppContextProvider value={appContext()}>
      <NewWorkspaceDialog open onOpenChange={fn()} repos={repos} />
    </AppContextProvider>
  );
}

const meta = {
  title: "Code/New workspace",
  component: NewWorkspace,
  parameters: { layout: "fullscreen" },
  decorators: [(Story) => withRouter(<Story />)],
} satisfies Meta<typeof NewWorkspace>;

export default meta;
type Story = StoryObj<typeof meta>;

/**
 * Sticky defaults answered, message focused, one Enter from a running task.
 * Every pill has a chord: Cmd+N repo, Alt+E engine, Alt+M model, Alt+P
 * permissions, Alt+B base ref, Alt+N name.
 */
export const Default: Story = {};

/**
 * Engines that need install or sign-in stay listed and dimmed in the engine
 * menu; a warm install for the missing pin reports under the message.
 */
export const EnginesNeedSetup: Story = {
  args: { doctor: harnessDoctorDegraded },
};
