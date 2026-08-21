// @vitest-environment jsdom
import { cleanup, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";

import { AppContextProvider, type AppContextValue } from "@/AppContext";
import type { ApiClient } from "../api/client";
import {
  deliveryPullRequests,
  deliveryRepositoriesSnapshot,
} from "../stories/fixtures";
import { CodeDeliveryPage } from "./CodeDeliveryPage";
import { useCodeDeliveryStore } from "./CodeDeliveryStore";

afterEach(() => {
  cleanup();
  useCodeDeliveryStore.getState().reset();
});

vi.mock("@/openInBrowser", () => ({ openInBrowser: vi.fn() }));
vi.mock("sonner", () => ({ toast: { success: vi.fn(), error: vi.fn() } }));
vi.mock("./CodeSidebar", () => ({ CodeSidebar: () => null }));

function storyClient(): ApiClient {
  return {
    getCodeDeliveryRepositories: async () => deliveryRepositoriesSnapshot,
    queryCodeDeliveryPullRequests: async () => ({
      capability: deliveryRepositoriesSnapshot.capability,
      // "All" — every lifecycle at once, which is the view the bug showed up in.
      items: deliveryPullRequests,
      errors: [],
      fetched_at: "2026-08-20T15:20:00.000Z",
    }),
  } as unknown as ApiClient;
}

function appContext(client: ApiClient): AppContextValue {
  return { client } as unknown as AppContextValue;
}

function renderList() {
  const rootRoute = createRootRoute();
  const route = createRoute({
    getParentRoute: () => rootRoute,
    path: "/code/delivery/pull-requests",
    component: () => <CodeDeliveryPage surface="pull_requests" />,
  });
  const router = createRouter({
    routeTree: rootRoute.addChildren([route]),
    history: createMemoryHistory({
      initialEntries: ["/code/delivery/pull-requests"],
    }),
  });
  render(
    <AppContextProvider value={appContext(storyClient())}>
      <RouterProvider router={router as never} />
    </AppContextProvider>,
  );
}

async function rowFor(title: string): Promise<HTMLElement> {
  const label = await screen.findByText(title, {}, { timeout: 3000 });
  const row = label.closest('[role="listitem"]');
  if (!(row instanceof HTMLElement)) throw new Error(`no row for ${title}`);
  return row;
}

describe("delivery pull request list", () => {
  // The reported bug: every settled pull request rendered "Review Pending",
  // because GitHub clears the review decision once one merges or closes.
  it("labels merged and closed pull requests by their outcome", async () => {
    renderList();
    expect(
      within(
        await rowFor("Cache the workspace digest between polls"),
      ).getByText("Merged"),
    ).toBeInTheDocument();
    expect(
      within(await rowFor("Rewrite the deployment runbook")).getByText(
        "Closed",
      ),
    ).toBeInTheDocument();
    // Merged, but the host reported CLOSED. Only the merge timestamp says so.
    expect(
      within(await rowFor("Split the workspace route")).getByText("Merged"),
    ).toBeInTheDocument();
    expect(screen.queryByText("Review Pending")).toBeNull();
  });

  it("still reports the review state on a live pull request", async () => {
    renderList();
    expect(
      within(await rowFor("Build the delivery center")).getByText(
        "Changes requested",
      ),
    ).toBeInTheDocument();
    expect(
      within(await rowFor("Make workspace deep links durable")).getByText(
        "Approved",
      ),
    ).toBeInTheDocument();
    expect(
      within(await rowFor("Document managed deployments")).getByText("Draft"),
    ).toBeInTheDocument();
  });

  it("names the lifecycle for a screen reader on every row", async () => {
    renderList();
    expect(
      within(
        await rowFor("Cache the workspace digest between polls"),
      ).getByText("Merged:"),
    ).toBeInTheDocument();
    expect(
      within(await rowFor("Rewrite the deployment runbook")).getByText(
        "Closed:",
      ),
    ).toBeInTheDocument();
  });

  it("keeps the ready-to-merge and attention marks apart", async () => {
    renderList();
    expect(
      within(await rowFor("Make workspace deep links durable")).getByLabelText(
        "Ready to merge",
      ),
    ).toBeInTheDocument();
    expect(
      within(await rowFor("Build the delivery center")).getByLabelText(
        "Needs attention",
      ),
    ).toBeInTheDocument();
    // A merged pull request is neither.
    const merged = await rowFor("Cache the workspace digest between polls");
    expect(within(merged).queryByLabelText("Ready to merge")).toBeNull();
    expect(within(merged).queryByLabelText("Needs attention")).toBeNull();
  });

  it("summarizes the checks per row", async () => {
    renderList();
    expect(
      within(await rowFor("Build the delivery center")).getByText("1 failed"),
    ).toBeInTheDocument();
    expect(
      within(await rowFor("Make workspace deep links durable")).getByText(
        "2 passed",
      ),
    ).toBeInTheDocument();
    expect(
      within(await rowFor("Document managed deployments")).getByText(
        "1 pending",
      ),
    ).toBeInTheDocument();
  });
});
