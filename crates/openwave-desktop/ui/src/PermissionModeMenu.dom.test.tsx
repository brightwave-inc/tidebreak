// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
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
