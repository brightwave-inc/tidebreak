// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { SidebarAccountMenuPanel } from "./SidebarAccountMenu";
import { railAccountIdentity } from "./railAccountIdentity";

afterEach(cleanup);

describe("SidebarAccountMenuPanel", () => {
  it("keeps the chip empty until an account exists", () => {
    render(
      <SidebarAccountMenuPanel
        identity={railAccountIdentity({ gateway: null })}
        themeMode="system"
        onSettings={vi.fn()}
        onThemeMode={vi.fn()}
      />,
    );
    expect(
      screen.getByRole("button", { name: "Account menu" }),
    ).toHaveTextContent("Account");
    expect(screen.queryByText("@")).not.toBeInTheDocument();
  });

  it("opens settings from the menu", async () => {
    const user = userEvent.setup();
    const onSettings = vi.fn();
    render(
      <SidebarAccountMenuPanel
        identity={railAccountIdentity({
          gateway: {
            signed_in: true,
            account_hint: "abaas@example.test",
          },
        })}
        themeMode="dark"
        onSettings={onSettings}
        onThemeMode={vi.fn()}
      />,
    );
    await user.click(screen.getByRole("button", { name: "Account menu" }));
    expect(screen.getAllByText("abaas@example.test").length).toBeGreaterThan(0);
    await user.click(screen.getByRole("menuitem", { name: "Settings" }));
    expect(onSettings).toHaveBeenCalledTimes(1);
  });
});
