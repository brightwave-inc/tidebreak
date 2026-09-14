// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  Outlet,
  RouterProvider,
} from "@tanstack/react-router";
import { afterEach, expect, it, vi } from "vitest";
const mocks = vi.hoisted(() => ({
  client: { listCodeGrants: vi.fn(), getPersonalInferencePreferences: vi.fn() },
}));
vi.mock("../AppContext", () => ({ useApp: () => ({ client: mocks.client }) }));
import { SETTINGS_SECTIONS } from "./sections";
afterEach(() => {
  cleanup();
  vi.resetAllMocks();
  vi.restoreAllMocks();
});
it("opens personal subscription settings from Channels and returns without stale search parameters", async () => {
  vi.spyOn(window, "scrollTo").mockImplementation(() => {});
  const grant = {
    id: "person-one",
    kind: "person",
    channel_kind: "slack",
    external_identity: "U-CASEY",
    display_name: "Casey",
    workspace_identity: "T-ACME",
    workspace_name: "Acme",
    created_at: "2026-09-14T12:00:00Z",
  };
  mocks.client.listCodeGrants.mockResolvedValue([grant]);
  mocks.client.getPersonalInferencePreferences.mockResolvedValue({
    dm_subscription_preference: "prefer_owned_subscription",
    channel_sponsorship_enabled: false,
    consent_version: null,
    inference_sponsorship_supported: true,
  });
  const section = SETTINGS_SECTIONS.find((entry) => entry.path === "channels")!;
  const root = createRootRoute({ component: Outlet });
  const channelRoute = createRoute({
    getParentRoute: () => root,
    path: "/settings/channels",
    component: section.Component,
    validateSearch: section.validateSearch,
  });
  const router = createRouter({
    routeTree: root.addChildren([channelRoute]),
    history: createMemoryHistory({ initialEntries: ["/settings/channels"] }),
    defaultPreload: false,
  });
  await router.load();
  render(<RouterProvider router={router} />);
  const user = userEvent.setup();
  await user.click(
    await screen.findByRole("button", { name: "Subscription settings" }),
  );
  await screen.findByRole("heading", { name: "Your Slack subscriptions" });
  expect(mocks.client.getPersonalInferencePreferences).toHaveBeenCalledWith(
    "person-one",
  );
  expect(router.state.location.search).toMatchObject({
    grant: "person-one",
    inference: "personal",
  });
  await user.click(screen.getByRole("button", { name: "Back to Channels" }));
  await screen.findByRole("heading", { name: "Channels" });
  await waitFor(() => expect(router.state.location.search).toEqual({}));
});
