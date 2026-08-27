import { useEffect, useState } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import { expect, within } from "storybook/test";

import { AppContextProvider, type AppContextValue } from "@/AppContext";
import type { ApiClient } from "@/api/client";
import type {
  CodeAnalyticsDay,
  CodeAnalyticsSnapshot,
  CodeRepoSnapshot,
  CodeSubscriptionUsage,
  HarnessKind,
} from "@/api/types";
import { CodeAnalyticsPage } from "@/code/CodeAnalyticsPage";
import { useCodeCatalogStore } from "@/code/CodeCatalogStore";
import { DEFAULT_RAIL_PREFS, useCodeUiStore } from "@/code/CodeUiStore";
import {
  disconnectCodeUpdates,
  useCodeUpdatesStore,
} from "@/code/CodeUpdatesStore";
import { resetCodeSubscriptionUsageStore } from "@/code/useCodeSubscriptionUsage";
import { SidebarExpandStrip } from "@/sidebar/SidebarExpandStrip";
import { useUiStore } from "@/UiStore";
import {
  deliveryCodeRepo,
  deliveryWorkspaces,
  harnessDoctor,
} from "./fixtures";

type AnalyticsScenario =
  | "gateway"
  | "local"
  | "loading"
  | "empty"
  | "failure"
  | "long";

function pending<T>(): Promise<T> {
  return new Promise(() => {});
}

function idleSocket(): WebSocket {
  const socket = {
    onopen: null as WebSocket["onopen"],
    onclose: null as WebSocket["onclose"],
    onerror: null as WebSocket["onerror"],
    close() {},
    addEventListener() {},
    removeEventListener() {},
  } as unknown as WebSocket;
  queueMicrotask(() => socket.onopen?.(new Event("open")));
  return socket;
}

const gatewayUsage: CodeSubscriptionUsage = {
  source: "model_gateway",
  diagnostics: [],
  providers: [
    {
      id: "anthropic",
      label: "Anthropic",
      accounts: [
        {
          id: "personal",
          label: "Personal",
          is_own: true,
          state: "available",
          updated_at_unix_seconds: 1787580900,
          windows: [
            {
              key: "five-hour",
              label: "5-hour",
              used_percent: 38,
              status: "allowed",
            },
            {
              key: "weekly",
              label: "Weekly",
              used_percent: 72,
              status: "allowed",
            },
          ],
        },
      ],
    },
    {
      id: "openai",
      label: "OpenAI",
      accounts: [
        {
          id: "team",
          label: "Team",
          is_own: false,
          state: "available",
          windows: [
            {
              key: "weekly",
              label: "Weekly",
              used_percent: 86,
              status: "allowed_warning",
            },
          ],
        },
      ],
    },
  ],
};

const localUsage: CodeSubscriptionUsage = {
  source: "unavailable",
  diagnostics: ["Model Gateway usage is unavailable."],
  providers: [],
};

const repoNames = [
  "tidebreak",
  "model-gateway",
  "desktop-shell",
  "docs",
  "release-tools",
  "design-system",
  "agent-runtime",
  "web-client",
];

const analyticsRepos: CodeRepoSnapshot[] = repoNames.map((name, index) => ({
  ...deliveryCodeRepo,
  id: `repo-analytics-${index + 1}`,
  display_name: name,
  root_path: `/Users/sam/code/${name}`,
  created_at: `2026-0${index < 4 ? 7 : 8}-${String(index + 4).padStart(2, "0")}T12:00:00.000Z`,
}));

function analyticsDays(empty = false): CodeAnalyticsDay[] {
  return Array.from({ length: 30 }, (_, index) => {
    const date = new Date(Date.UTC(2026, 6, 26 + index));
    const wave = [0.42, 0.78, 0.56, 1, 0.7, 0.22, 0.1][index % 7] ?? 0;
    const tokens = empty ? 0 : Math.round((1_100_000 + index * 31_000) * wave);
    return {
      date: date.toISOString().slice(0, 10),
      sessions: empty ? 0 : Math.max(0, Math.round(5 * wave)),
      turns: empty ? 0 : Math.max(0, Math.round(18 * wave)),
      total_tokens: tokens,
      estimated_cost_microusd: empty ? 0 : Math.round(tokens * 2.85),
      pull_requests_opened: empty || index % 5 !== 0 ? 0 : 1,
      pull_requests_merged: empty || index % 7 !== 0 ? 0 : 1,
    };
  });
}

function analyticsReport(scenario: AnalyticsScenario): CodeAnalyticsSnapshot {
  const empty = scenario === "empty";
  const long = scenario === "long";
  const repositories = (long ? analyticsRepos : analyticsRepos.slice(0, 4)).map(
    (repo, index) => ({
      repo_id: repo.id,
      name: repo.display_name,
      sessions: empty ? 0 : 31 - index * 2,
      turns: empty ? 0 : 128 - index * 9,
      total_tokens: empty ? 0 : 12_800_000 - index * 1_050_000,
      estimated_cost_microusd: empty ? 0 : 34_900_000 - index * 2_300_000,
      pull_requests_opened: empty ? 0 : Math.max(1, 12 - index),
      pull_requests_merged: empty ? 0 : Math.max(0, 9 - index),
    }),
  );
  const baseModels = [
    ["claude-opus-5", "claude_code", false, true, 17_400_000, 57_600_000],
    ["gpt-5.6-sol", "codex", false, true, 12_600_000, 45_200_000],
    ["claude-sonnet-5", "opencode", false, true, 7_900_000, 12_800_000],
    ["gpt-5.6-luna", "codex", false, true, 3_300_000, 1_250_000],
    ["anthropic-us-claude-opus-5", "claude_code", true, false, 2_100_000, 0],
    ["deepseek-v4-flash-0731", "opencode", false, false, 1_700_000, 0],
  ] as const;
  const modelRows = long
    ? [
        ...baseModels,
        ...Array.from(
          { length: 9 },
          (_, index) =>
            [
              `provider/model-${index + 1}`,
              (["codex", "opencode", "grok"] as const)[index % 3],
              index % 4 === 0,
              false,
              1_400_000 - index * 90_000,
              0,
            ] as const,
        ),
      ]
    : baseModels;
  const models = modelRows.map(
    (
      [model_id, harness_kind, fast_mode, priced, total_tokens, cost],
      index,
    ) => ({
      model_id,
      harness_kind: harness_kind as HarnessKind,
      fast_mode,
      sessions: empty ? 0 : Math.max(2, 24 - index * 2),
      turns: empty ? 0 : Math.max(4, 92 - index * 8),
      total_tokens: empty ? 0 : total_tokens,
      estimated_cost_microusd: empty ? 0 : cost,
      priced,
    }),
  );
  return {
    range: "30d",
    from: "2026-07-26T16:00:00.000Z",
    through: "2026-08-24T16:00:00.000Z",
    totals: {
      sessions: empty ? 0 : 84,
      turns: empty ? 0 : 412,
      completed_turns: empty ? 0 : 371,
      failed_turns: empty ? 0 : 18,
      interrupted_turns: empty ? 0 : 15,
      running_turns: empty ? 0 : 8,
      input_tokens: empty ? 0 : 8_400_000,
      output_tokens: empty ? 0 : 3_100_000,
      cache_read_tokens: empty ? 0 : 31_700_000,
      cache_write_tokens: empty ? 0 : 1_800_000,
      total_tokens: empty ? 0 : 45_000_000,
      estimated_cost_microusd: empty ? 0 : 116_850_000,
      pull_requests_opened: empty ? 0 : 27,
      pull_requests_merged: empty ? 0 : 21,
    },
    daily: analyticsDays(empty),
    repositories,
    models,
    harnesses: empty
      ? []
      : [
          {
            harness_kind: "claude_code",
            sessions: 38,
            turns: 186,
            total_tokens: 21_300_000,
            estimated_cost_microusd: 57_600_000,
          },
          {
            harness_kind: "codex",
            sessions: 29,
            turns: 144,
            total_tokens: 15_900_000,
            estimated_cost_microusd: 46_450_000,
          },
          {
            harness_kind: "opencode",
            sessions: 17,
            turns: 82,
            total_tokens: 7_800_000,
            estimated_cost_microusd: 12_800_000,
          },
        ],
    pricing: {
      priced_turns: empty ? 0 : 356,
      unpriced_turns: empty ? 0 : 48,
      priced_tokens: empty ? 0 : 40_200_000,
      unpriced_tokens: empty ? 0 : 4_800_000,
      prices_as_of: "2026-08-21",
    },
  };
}

function storyClient(scenario: AnalyticsScenario): ApiClient {
  const usage = scenario === "gateway" ? gatewayUsage : localUsage;
  return {
    openCodeUpdates: () => idleSocket(),
    listCodeRepos: async () =>
      scenario === "long" ? analyticsRepos : analyticsRepos.slice(0, 4),
    listCodeWorkspaces: async () => deliveryWorkspaces,
    getHarnessDoctor: async () => harnessDoctor,
    listCodeHarnessModels: async (kind: HarnessKind) => ({ kind, models: [] }),
    getCodeSubscriptionUsage: async () => usage,
    getCodeAnalytics: async () => {
      if (scenario === "loading") return pending();
      if (scenario === "failure")
        throw new Error("The local database is busy.");
      return analyticsReport(scenario);
    },
    getCodeCloneDefaults: async () => ({
      gh_found: true,
      gh_authenticated: true,
      gh_remediation: "",
    }),
    startCodeWatch: async () => ({}) as never,
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

function storyRouter() {
  const rootRoute = createRootRoute();
  const analyticsRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/code/analytics",
    component: CodeAnalyticsPage,
  });
  const placeholders = [
    "/",
    "/code",
    "/code/delivery/pull-requests",
    "/code/archive",
    "/settings",
  ].map((path) =>
    createRoute({
      getParentRoute: () => rootRoute,
      path,
      component: () => <p className="p-6">{path}</p>,
    }),
  );
  const workspaceRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/code/w/$workspaceId",
    component: () => <p className="p-6">Workspace</p>,
  });
  return createRouter({
    routeTree: rootRoute.addChildren([
      ...placeholders,
      workspaceRoute,
      analyticsRoute,
    ]),
    history: createMemoryHistory({ initialEntries: ["/code/analytics"] }),
  });
}

function resetStoryState(sidebarCollapsed: boolean) {
  disconnectCodeUpdates();
  useCodeCatalogStore.getState().reset();
  useCodeUpdatesStore.getState().reset();
  resetCodeSubscriptionUsageStore();
  useCodeUiStore.setState({
    railPrefs: DEFAULT_RAIL_PREFS,
    reviewSidebarOpen: false,
    newWorkspaceOpen: false,
    addRepoOpen: false,
    pendingComposerPrompt: null,
    composerActionScope: null,
  });
  useUiStore.setState({ sidebarCollapsed, sidebarWidth: 280 });
}

function CodeAnalyticsStory({
  scenario,
  sidebarCollapsed = false,
}: {
  scenario: AnalyticsScenario;
  sidebarCollapsed?: boolean;
}) {
  const [state] = useState(() => {
    resetStoryState(sidebarCollapsed);
    const client = storyClient(scenario);
    return { client, router: storyRouter() };
  });
  useEffect(
    () => () => {
      disconnectCodeUpdates();
      useCodeCatalogStore.getState().reset();
      useCodeUpdatesStore.getState().reset();
      resetCodeSubscriptionUsageStore();
    },
    [],
  );
  return (
    <AppContextProvider value={appContext(state.client)}>
      <div className="app-shell h-full min-h-0 w-full overflow-hidden">
        <SidebarExpandStrip macOverlay />
        <div className="app-body">
          <RouterProvider router={state.router as never} />
        </div>
      </div>
    </AppContextProvider>
  );
}

const meta = {
  title: "Code/Analytics",
  component: CodeAnalyticsStory,
  args: { scenario: "gateway", sidebarCollapsed: false },
  parameters: { layout: "fullscreen" },
  render: (args) => (
    <CodeAnalyticsStory
      key={`${args.scenario}:${args.sidebarCollapsed}`}
      {...args}
    />
  ),
} satisfies Meta<typeof CodeAnalyticsStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const WithModelGateway: Story = {};

export const LocalOnly: Story = {
  args: { scenario: "local" },
};

export const Loading: Story = {
  args: { scenario: "loading" },
};

export const EmptyState: Story = {
  args: { scenario: "empty" },
};

export const Failure: Story = {
  args: { scenario: "failure" },
};

export const LongContent: Story = {
  args: { scenario: "long" },
};

export const CollapsedMacRail: Story = {
  args: { scenario: "gateway", sidebarCollapsed: true },
  globals: { viewport: { value: "minimumWindow", isRotated: false } },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const strip = canvasElement.querySelector<HTMLElement>(
      ".sidebar-expand-strip",
    );
    const heading = await canvas.findByRole("heading", { name: "Analytics" });
    const header = heading.closest("header");
    await expect(strip).toBeVisible();
    await expect(header).toBeTruthy();
    await expect(
      header?.getBoundingClientRect().top ?? 0,
    ).toBeGreaterThanOrEqual((strip?.getBoundingClientRect().bottom ?? 0) - 1);
  },
};

export const NarrowWidth: Story = {
  args: { scenario: "gateway" },
  parameters: {
    viewport: { defaultViewport: "tablet" },
  },
};
