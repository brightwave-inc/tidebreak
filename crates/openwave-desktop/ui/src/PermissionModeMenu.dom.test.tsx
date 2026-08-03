// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import type { ManagedPolicy } from "./api";
import { ManagedPolicyContext } from "./managedPolicy";
import { PermissionModeMenu } from "./PermissionModeMenu";

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

/**
 * The menu used to hold itself open after a choice, which reads as a dropped
 * click on a control whose whole job is one decision.
 */
it("applies the chosen mode and closes", async () => {
  const onChange = vi.fn();
  render(<PermissionModeMenu value={null} onChange={onChange} />);

  await userEvent.click(screen.getByRole("button", { name: "Permissions: Ask" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: /Allow all/ }));

  expect(onChange).toHaveBeenCalledWith("allow");
  await waitFor(() =>
    expect(screen.queryByRole("menuitem", { name: /Allow all/ })).not.toBeInTheDocument(),
  );
});

/**
 * #923: a managed permission-mode ceiling renders over-ceiling modes as
 * locked — visible but unselectable — and a stored mode above the ceiling
 * displays as the ceiling the turn actually runs under.
 */
it("locks modes above a managed ceiling", async () => {
  const capped: ManagedPolicy = {
    managed: false,
    source: "unmanaged",
    misconfigured: false,
    permission_mode_ceiling: "ask",
  };
  const onChange = vi.fn();
  render(
    <ManagedPolicyContext.Provider value={capped}>
      <PermissionModeMenu value="allow" onChange={onChange} />
    </ManagedPolicyContext.Provider>,
  );

  // The over-ceiling stored mode reads as its clamped effective mode.
  await userEvent.click(screen.getByRole("button", { name: "Permissions: Ask" }));
  const locked = await screen.findByRole("menuitem", { name: /Allow all/ });
  expect(locked).toHaveAttribute("aria-disabled", "true");
  expect(locked).toHaveTextContent("Locked by your organization's policy.");
  expect(
    screen.getByRole("menuitem", { name: /Plan/ }),
  ).not.toHaveAttribute("aria-disabled", "true");

  await userEvent.click(locked);
  expect(onChange).not.toHaveBeenCalled();
});
