// @vitest-environment jsdom

import { cleanup, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useUiStore } from "@/UiStore";
import { SettingsSidebar } from "./SettingsSidebar";

const router = vi.hoisted(() => ({
  navigate: vi.fn(),
  pathname: "/settings/connected-apps",
}));

vi.mock("@tanstack/react-router", () => ({
  useNavigate: () => router.navigate,
  useRouterState: ({
    select,
  }: {
    select: (state: { location: { pathname: string } }) => unknown;
  }) => select({ location: { pathname: router.pathname } }),
}));

afterEach(() => {
  cleanup();
  router.navigate.mockClear();
});

describe("SettingsSidebar", () => {
  it("labels each visible settings group", () => {
    useUiStore.setState({ sidebarCollapsed: false, sidebarWidth: 280 });
    render(<SettingsSidebar onBack={() => {}} />);

    const navigation = screen.getByRole("navigation", {
      name: "Settings sections",
    });
    const models = within(navigation).getByRole("group", {
      name: "Models & agents",
    });
    const capabilities = within(navigation).getByRole("group", {
      name: "Capabilities",
    });
    const application = within(navigation).getByRole("group", {
      name: "Application",
    });

    expect(
      within(models).getByRole("button", { name: "Providers" }),
    ).toBeVisible();
    expect(
      within(capabilities).getByRole("button", { name: "Connected apps" }),
    ).toHaveAttribute("aria-current", "page");
    expect(
      within(application).getByRole("button", { name: "Updates" }),
    ).toBeVisible();
    expect(
      within(application).getByRole("button", {
        name: "Git & source control",
      }),
    ).toBeVisible();
    expect(
      within(application).getByRole("button", { name: "Experimental" }),
    ).toBeVisible();
    expect(
      within(capabilities).queryByRole("button", { name: "Memory" }),
    ).toBeNull();
  });
});
