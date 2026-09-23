// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { SettingsField } from "./primitives";

afterEach(cleanup);

describe("SettingsField", () => {
  it("keeps the hint out of the control name and does not activate on hint click", async () => {
    const onChange = vi.fn();
    render(
      <SettingsField label="Completion notifications" hint="Show a toast.">
        <input type="checkbox" onChange={onChange} />
      </SettingsField>,
    );

    const control = screen.getByRole("checkbox", {
      name: "Completion notifications",
    });
    expect(control).toHaveAccessibleDescription("Show a toast.");
    await userEvent.click(screen.getByText("Show a toast."));
    expect(onChange).not.toHaveBeenCalled();
  });
});
