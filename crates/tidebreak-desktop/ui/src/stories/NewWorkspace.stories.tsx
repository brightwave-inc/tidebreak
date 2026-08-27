import { useEffect, useState, type ReactNode } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, fn, userEvent, within } from "storybook/test";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";

import { AppContextProvider, type AppContextValue } from "@/AppContext";
import type {
  CodeRepoSnapshot,
  HarnessKind,
  ManagedPolicy,
  PermissionMode,
  ReasoningEffort,
} from "@/api/types";
import { useCodeCatalogStore } from "@/code/CodeCatalogStore";
import { EMPTY_NEW_WORKSPACE_DRAFT, useCodeUiStore } from "@/code/CodeUiStore";
import { useCodeUpdatesStore } from "@/code/CodeUpdatesStore";
import { NewWorkspaceDialog } from "@/code/NewWorkspaceDialog";
import { ManagedPolicyContext } from "@/managedPolicy";
import {
  harnessDoctor,
  harnessDoctorCold,
  harnessDoctorDegraded,
  harnessInstallsInFlight,
} from "./fixtures";

/**
 * The new-workspace composer: the first message is the surface, and every
 * setting — repo, name, base ref, engine, model, reasoning, fast mode,
 * permissions — is a pill with its own chord. Enter creates; "Create more"
 * keeps it open for the next one.
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
  Record<
    HarnessKind,
    {
      id: string;
      label: string;
      default?: boolean;
      reasoning_efforts: ReasoningEffort[];
      fast_mode: boolean;
    }[]
  >
> = {
  claude_code: [
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
      reasoning_efforts: [],
      fast_mode: false,
    },
  ],
  codex: [
    {
      id: "gpt-5.6-sol",
      label: "GPT 5.6 Sol",
      default: true,
      reasoning_efforts: ["low", "medium", "high"],
      fast_mode: true,
    },
    {
      id: "gpt-5.6-luna",
      label: "GPT 5.6 Luna",
      reasoning_efforts: ["low", "medium", "high"],
      fast_mode: false,
    },
  ],
  opencode: [
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
      reasoning_efforts: [],
      fast_mode: false,
    },
    {
      id: "grok-4.5",
      label: "Grok 4.5",
      reasoning_efforts: [],
      fast_mode: false,
    },
  ],
  grok: [
    {
      id: "grok-4.6",
      label: "Grok 4.6",
      default: true,
      reasoning_efforts: ["low", "medium", "high", "xhigh"],
      fast_mode: false,
    },
  ],
};

function appContext(): AppContextValue {
  const client = {
    listCodeHarnessModels: async (kind: HarnessKind) => ({
      kind,
      models: MODELS[kind] ?? [],
      reasoning_efforts:
        kind === "claude_code"
          ? (["low", "medium", "high"] as ReasoningEffort[])
          : kind === "codex"
            ? (["low", "medium", "high"] as ReasoningEffort[])
            : kind === "grok"
              ? (["low", "medium", "high", "xhigh"] as ReasoningEffort[])
              : [],
      fast_mode: kind === "claude_code" || kind === "codex",
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
  installs,
  initialHarness,
  permissionModeCeiling,
}: {
  doctor?: typeof harnessDoctor;
  installs?: typeof harnessInstallsInFlight;
  initialHarness?: HarnessKind;
  permissionModeCeiling?: PermissionMode;
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
    useCodeUpdatesStore.setState({ harnessInstalls: installs ?? {} });
    useCodeUiStore.setState({
      lastCreate: initialHarness
        ? { harness: initialHarness, modelsByHarness: {} }
        : null,
      newWorkspaceDraft: EMPTY_NEW_WORKSPACE_DRAFT,
    });
    setSeeded(true);
  }, [doctor, initialHarness, installs]);
  if (!seeded) return null;
  const policy: ManagedPolicy = {
    managed: Boolean(permissionModeCeiling),
    source: permissionModeCeiling ? "os" : "unmanaged",
    misconfigured: false,
    allow_local_mcp_servers: false,
    permission_mode_ceiling: permissionModeCeiling,
  };
  return (
    <ManagedPolicyContext.Provider value={policy}>
      <AppContextProvider value={appContext()}>
        <NewWorkspaceDialog open onOpenChange={fn()} repos={repos} />
      </AppContextProvider>
    </ManagedPolicyContext.Provider>
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

/** One CSS rem, which the interface zoom moves. Sizes below scale with it. */
function rem(element: HTMLElement): number {
  return Number.parseFloat(
    getComputedStyle(element.ownerDocument.documentElement).fontSize,
  );
}

/**
 * Sticky defaults answered, message focused, one Enter from a running task.
 * Every pill has a chord: Cmd+N repo, Alt+E engine, Alt+M model, Alt+P
 * permissions, Alt+B base ref, Alt+N name.
 */
export const Default: Story = {
  play: async ({ canvasElement }) => {
    const page = within(canvasElement.ownerDocument.body);
    const dialog = await page.findByRole("dialog");
    const prompt = page.getByRole("textbox", { name: "First message" });
    const unit = rem(dialog);
    await expect(dialog).toBeVisible();
    // `max-w-4xl` on the dialog and `sm:min-h-52` on the prompt, in pixels.
    expect(dialog.getBoundingClientRect().width).toBeCloseTo(56 * unit, 0);
    expect(prompt.getBoundingClientRect().height).toBeGreaterThanOrEqual(
      13 * unit,
    );
  },
};

/** The minimum supported window wraps settings without clipping the actions. */
export const MinimumWindow: Story = {
  globals: { viewport: { value: "minimumWindow", isRotated: false } },
  play: async ({ canvasElement }) => {
    const page = within(canvasElement.ownerDocument.body);
    const dialog = await page.findByRole("dialog");
    const create = page.getByRole("button", { name: /Create/ });
    await expect(create).toBeVisible();
    expect(dialog.scrollWidth).toBeLessThanOrEqual(dialog.clientWidth);
    // The wrapped footer is the row a short window used to cut off.
    expect(dialog.scrollHeight).toBeLessThanOrEqual(dialog.clientHeight);
    expect(create.getBoundingClientRect().bottom).toBeLessThanOrEqual(
      dialog.getBoundingClientRect().bottom,
    );
  },
};

/** Picking the repo is the one setting above the message, and it has a chord. */
export const RepoPicker: Story = {
  play: async ({ canvasElement }) => {
    const page = within(canvasElement.ownerDocument.body);
    await userEvent.click(await page.findByRole("button", { name: "Repo" }));
    const menu = await page.findByRole("menu");
    await expect(within(menu).getByText("toronto")).toBeVisible();
    await expect(menu.getBoundingClientRect().bottom).toBeLessThanOrEqual(
      window.innerHeight,
    );
  },
};

/** The model list belongs to the engine, so it opens upward from the footer. */
export const ModelPicker: Story = {
  play: async ({ canvasElement }) => {
    const page = within(canvasElement.ownerDocument.body);
    await userEvent.click(
      await page.findByRole("button", { name: /Claude Opus 5/ }),
    );
    const menu = await page.findByRole("menu");
    await expect(within(menu).getByText("Claude Sonnet 5")).toBeVisible();
    await expect(menu.getBoundingClientRect().top).toBeGreaterThanOrEqual(0);
  },
};

/** The base ref is a free-text field, not a branch list: tags and commits count. */
export const BaseRefPicker: Story = {
  play: async ({ canvasElement }) => {
    const page = within(canvasElement.ownerDocument.body);
    await userEvent.click(
      await page.findByRole("button", { name: "Base ref" }),
    );
    await expect(
      await page.findByRole("textbox", { name: "Base ref" }),
    ).toBeVisible();
    await expect(
      page.getByText("The branch, tag, or commit the worktree is cut from."),
    ).toBeVisible();
  },
};

/**
 * Engines that cannot be fixed by waiting — no pin, or signed out — stay
 * listed and dimmed in the engine menu with the one reason why.
 */
export const EnginesNeedSetup: Story = {
  args: { doctor: harnessDoctorDegraded },
  play: async ({ canvasElement }) => {
    const page = within(canvasElement.ownerDocument.body);
    await userEvent.click(
      await page.findByRole("button", { name: /^Harness:/ }),
    );
    const menu = await page.findByRole("menu");
    await expect(within(menu).getByText("Coding harnesses…")).toBeVisible();
    await expect(menu.getBoundingClientRect().top).toBeGreaterThanOrEqual(0);
  },
};

/**
 * A machine with nothing downloaded. The engine menu still offers all four,
 * because picking one is what fetches it; Create waits for the pin, and the
 * line under the message says what the wait is.
 */
export const EngineDownloading: Story = {
  args: {
    doctor: harnessDoctorCold,
    installs: {
      claude_code: {
        kind: "claude_code",
        version: "2.1.234",
        phase: "installing",
        done: false,
      },
    },
  },
};

/** The selected engine has no mode at or below the managed ceiling. */
export const BlockedByManagedCeiling: Story = {
  args: {
    doctor: {
      ...harnessDoctor,
      harnesses: harnessDoctor.harnesses
        .filter((entry) => entry.kind === "grok")
        .map((entry) => ({ ...entry, authenticated: true, remediation: "" })),
    },
    initialHarness: "grok",
    permissionModeCeiling: "ask",
  },
  play: async ({ canvasElement }) => {
    const page = within(canvasElement.ownerDocument.body);
    await expect(
      page.findByText(
        "This engine has no permission mode allowed by your organization's policy.",
      ),
    ).resolves.toBeVisible();
    await expect(page.getByRole("button", { name: /Create/ })).toBeDisabled();
  },
};
