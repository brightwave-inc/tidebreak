// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { UncleanExitNotice } from "./UncleanExitNotice";

afterEach(cleanup);

describe("UncleanExitNotice", () => {
  it("says the app quit unexpectedly and offers to save a report", async () => {
    const user = userEvent.setup();
    const onSave = vi.fn();
    const onDismiss = vi.fn();
    render(
      <UncleanExitNotice
        save={{ status: "idle" }}
        onSave={onSave}
        onDismiss={onDismiss}
      />,
    );

    expect(
      screen.getByRole("complementary", {
        name: "Tidebreak quit unexpectedly",
      }),
    ).toHaveTextContent("It stays on this computer unless you share it.");
    await user.click(
      screen.getByRole("button", { name: "Save diagnostics report" }),
    );
    await user.click(screen.getByRole("button", { name: "Dismiss notice" }));
    expect(onSave).toHaveBeenCalledOnce();
    expect(onDismiss).toHaveBeenCalledOnce();
  });

  it("holds the button while the report saves, then says it saved", () => {
    const props = { onSave: vi.fn(), onDismiss: vi.fn() };
    const { rerender } = render(
      <UncleanExitNotice save={{ status: "saving" }} {...props} />,
    );
    expect(
      screen.getByRole("button", { name: "Saving report…" }),
    ).toBeDisabled();

    rerender(<UncleanExitNotice save={{ status: "saved" }} {...props} />);
    expect(screen.getByRole("status")).toHaveTextContent("Report saved");
  });

  it("shows why a save failed and lets the person try again", () => {
    render(
      <UncleanExitNotice
        save={{
          status: "failed",
          error: "Error: Could not build the diagnostics report",
        }}
        onSave={vi.fn()}
        onDismiss={vi.fn()}
      />,
    );
    expect(screen.getByRole("alert")).toHaveTextContent(
      "Could not build the diagnostics report",
    );
    expect(screen.getByRole("alert")).not.toHaveTextContent("Error:");
    expect(
      screen.getByRole("button", { name: "Save diagnostics report" }),
    ).toBeEnabled();
  });
});
