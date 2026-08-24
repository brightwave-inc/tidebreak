import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, fn, within } from "storybook/test";

import type { AppDetail, AppGrantState, AppSummary } from "@/api";
import { AppDetailView } from "@/apps/AppDetailView";
import { AppsView } from "@/apps/AppsView";
import type { AppsApis } from "@/apps/appsApis";
import { ManagedPolicyContext } from "@/managedPolicy";

const appRows: AppSummary[] = [
  {
    id: "release-brief",
    name: "Release brief",
    revision_count: 4,
    updated_at: "2026-08-23T16:40:00.000Z",
    granted: true,
  },
  {
    id: "incident-map",
    name: "Incident map",
    revision_count: 1,
    updated_at: "2026-08-22T09:15:00.000Z",
    granted: false,
  },
  {
    id: "research-table",
    name: "Research source table with a deliberately long title",
    revision_count: 7,
    updated_at: "2026-08-19T18:20:00.000Z",
    granted: true,
  },
  {
    id: "cost-model",
    name: "Provider cost model",
    revision_count: 2,
    updated_at: "2026-08-17T12:05:00.000Z",
    granted: false,
  },
  {
    id: "launch-checklist",
    name: "Launch checklist",
    revision_count: 3,
    updated_at: "2026-08-12T08:30:00.000Z",
    granted: true,
  },
];

const detail: AppDetail = {
  id: "release-brief",
  name: "Release brief",
  created_at: "2026-08-18T10:10:00.000Z",
  updated_at: "2026-08-23T16:40:00.000Z",
  current_revision: "revision-4",
  revisions: [
    {
      id: "revision-4",
      ordinal: 4,
      created_at: "2026-08-23T16:40:00.000Z",
    },
    {
      id: "revision-3",
      ordinal: 3,
      created_at: "2026-08-22T15:20:00.000Z",
    },
    {
      id: "revision-2",
      ordinal: 2,
      created_at: "2026-08-20T13:00:00.000Z",
    },
    {
      id: "revision-1",
      ordinal: 1,
      created_at: "2026-08-18T10:10:00.000Z",
    },
  ],
};

const consentRequired: AppGrantState = {
  granted: false,
  bindings: [
    {
      app: null,
      folder: "folder-release-notes",
      gateway_app: null,
      access: "read_write",
      name: "Release notes",
      operation_ids: null,
      granted: true,
      definition_changed: true,
    },
    {
      app: null,
      folder: null,
      gateway_app: "github",
      access: null,
      name: "GitHub",
      operation_ids: ["pulls.list", "pulls.comment", "checks.read"],
      granted: false,
      definition_changed: false,
    },
  ],
};

const granted: AppGrantState = {
  granted: true,
  bindings: consentRequired.bindings.map((binding) => ({
    ...binding,
    granted: true,
    definition_changed: false,
  })),
};

function pending<T>(): Promise<T> {
  return new Promise(() => undefined);
}

function appApis({
  apps = appRows,
  appDetail = detail,
  grant = granted,
  listError,
  detailError,
  frame = "ready",
}: {
  apps?: AppSummary[];
  appDetail?: AppDetail;
  grant?: AppGrantState;
  listError?: Error;
  detailError?: Error;
  frame?: "ready" | "loading" | "failed";
} = {}): AppsApis {
  return {
    baseUrl: "",
    list: async () => {
      if (listError) throw listError;
      return { apps };
    },
    get: async () => {
      if (detailError) throw detailError;
      return appDetail;
    },
    deleteApp: fn(async () => {}),
    grantState: async () => grant,
    consent: async () => granted,
    revoke: fn(async () => {}),
    viewSession: async () => {
      if (frame === "loading") return pending();
      if (frame === "failed") throw new Error("stored bundle is unavailable");
      const markup = encodeURIComponent(`<!doctype html>
        <html><body style="margin:0;background:#f4f1e8;color:#25241f;font:14px system-ui">
        <main style="padding:28px;display:grid;gap:18px">
          <header><div style="font-size:12px;color:#6f6b61">RELEASE BRIEF</div>
          <h1 style="font-size:26px;margin:5px 0 0">Desktop release readiness</h1></header>
          <section style="display:grid;grid-template-columns:repeat(3,1fr);gap:12px">
            <article style="background:white;padding:16px;border-radius:10px"><b>17</b><div>checks passed</div></article>
            <article style="background:white;padding:16px;border-radius:10px"><b>2</b><div>reviews open</div></article>
            <article style="background:white;padding:16px;border-radius:10px"><b>1</b><div>release blocker</div></article>
          </section>
          <section style="background:white;padding:18px;border-radius:10px">
            <b>Next decision</b><p style="color:#6f6b61">Confirm the updater migration before the release branch is cut.</p>
          </section>
        </main></body></html>`);
      return { frame_path: `data:text/html;charset=utf-8,${markup}` };
    },
    invokeOperation: async () => ({ is_error: false }),
    invokeGatewayOperation: async () => ({ is_error: false }),
    invokeFolder: async () => ({ is_error: false }),
    gatewayBaseUrl: async () => "https://gateway.example.com",
    gatewayPage: async () => ({
      outcome: "ready",
      url: "https://gateway.example.com/apps/release-brief",
    }),
  };
}

const meta = {
  title: "Apps/Library",
  component: AppsView,
  parameters: { layout: "fullscreen" },
  decorators: [
    (Story) => (
      <div className="flex h-screen min-h-0 bg-page-background">
        <Story />
      </div>
    ),
  ],
  args: {
    apis: appApis(),
    onOpen: fn(),
  },
} satisfies Meta<typeof AppsView>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Loading: Story = {
  args: {
    apis: { ...appApis(), list: pending },
  },
};

export const Empty: Story = {
  args: { apis: appApis({ apps: [] }) },
};

export const DenseLibrary: Story = {
  play: async ({ canvasElement }) => {
    const list = within(canvasElement).getByRole("list", { name: "Apps" });
    await expect(list).toBeVisible();
    expect(list.getBoundingClientRect().width).toBeLessThanOrEqual(800);
    await expect(
      within(canvasElement).getAllByText("Access allowed")[0],
    ).toBeVisible();
  },
};

export const LoadFailure: Story = {
  args: {
    apis: appApis({
      apps: [],
      listError: new Error("The app library did not answer."),
    }),
  },
};

export const ConsentRequired: Story = {
  render: () => (
    <AppDetailView
      appId={detail.id}
      apis={appApis({ grant: consentRequired })}
      onBack={fn()}
    />
  ),
};

export const GrantedApp: Story = {
  render: () => (
    <ManagedPolicyContext.Provider
      value={{
        managed: true,
        source: "provisioned",
        misconfigured: false,
        allow_local_mcp_servers: false,
        gateway_url: "https://gateway.example.com",
      }}
    >
      <AppDetailView appId={detail.id} apis={appApis()} onBack={fn()} />
    </ManagedPolicyContext.Provider>
  ),
};

export const AppOpening: Story = {
  render: () => (
    <AppDetailView
      appId={detail.id}
      apis={appApis({ frame: "loading" })}
      onBack={fn()}
    />
  ),
};

export const AppFrameFailure: Story = {
  render: () => (
    <AppDetailView
      appId={detail.id}
      apis={appApis({ frame: "failed" })}
      onBack={fn()}
    />
  ),
};

export const DetailFailure: Story = {
  render: () => (
    <AppDetailView
      appId={detail.id}
      apis={appApis({ detailError: new Error("404: app not found") })}
      onBack={fn()}
    />
  ),
};
