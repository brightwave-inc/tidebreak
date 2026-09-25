// @vitest-environment jsdom
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { HttpError } from "../api/client/http";
import { friendlyErrorMessage, UNREACHABLE_SERVER_MESSAGE } from "../lib/utils";
import {
  SettingsError,
  SettingsField,
  SettingsStatus,
  type SettingsStatusTone,
} from "./primitives";

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

describe("SettingsError", () => {
  it("shows the message the formatter worded, and words it no further", () => {
    render(
      <>
        <SettingsError>
          {friendlyErrorMessage(
            new HttpError(409, "409: That name is taken."),
            "fallback",
          )}
        </SettingsError>
        <SettingsError>
          {friendlyErrorMessage(new TypeError("Failed to fetch"), "fallback")}
        </SettingsError>
        {/* Wording that merely looks like an error name stays as written. */}
        <SettingsError>Errors: two fields are empty.</SettingsError>
      </>,
    );

    expect(
      screen.getAllByRole("alert").map((line) => line.textContent),
    ).toEqual([
      "That name is taken.",
      UNREACHABLE_SERVER_MESSAGE,
      "Errors: two fields are empty.",
    ]);
  });

  it("is a critical notice, with a retry only when the load can run again", async () => {
    const onRetry = vi.fn();
    render(
      <>
        <SettingsError>Could not save.</SettingsError>
        <SettingsError onRetry={onRetry}>Could not load.</SettingsError>
      </>,
    );

    const [saveFailure, loadFailure] = screen.getAllByRole("alert");
    expect(saveFailure).toHaveAttribute("data-tone", "critical");
    expect(within(saveFailure).queryByRole("button")).toBeNull();
    await userEvent.click(
      within(loadFailure).getByRole("button", { name: "Try again" }),
    );
    expect(onRetry).toHaveBeenCalledOnce();
  });
});

describe("SettingsStatus", () => {
  it.each<[SettingsStatusTone, string]>([
    ["ready", "success"],
    ["neutral", "neutral"],
    ["warning", "warning"],
    ["critical", "critical"],
  ])("draws the %s tone as a %s notice", (tone, noticeTone) => {
    render(<SettingsStatus tone={tone} label="Verdict" description="Why." />);

    expect(screen.getByRole("status")).toHaveAttribute("data-tone", noticeTone);
  });
});

describe("SettingsField error", () => {
  it("puts validation under its field as text, marks the control, and offers no retry", () => {
    render(
      <SettingsField
        label="Timeout (seconds)"
        error="Timeout must be between 1 and 60 seconds."
      >
        <input />
      </SettingsField>,
    );

    const message = screen.getByRole("alert");
    expect(message).toHaveTextContent(
      "Timeout must be between 1 and 60 seconds.",
    );
    expect(message).not.toHaveAttribute("data-slot", "notice");
    expect(within(message).queryByRole("button")).toBeNull();
    const control = screen.getByRole("textbox", { name: "Timeout (seconds)" });
    expect(control).toHaveAttribute("aria-invalid", "true");
    expect(control.getAttribute("aria-describedby")).toContain(message.id);
  });
});
