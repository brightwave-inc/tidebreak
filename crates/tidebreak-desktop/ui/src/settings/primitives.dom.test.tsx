// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { HttpError } from "../api/client/http";
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
  it("shows the message without the name String(err) puts in front", () => {
    render(
      <>
        <SettingsError>
          {String(new Error("Could not reach the gateway."))}
        </SettingsError>
        <SettingsError>
          {String(new HttpError(409, "409: That name is taken."))}
        </SettingsError>
        <SettingsError>
          {String(new TypeError("Failed to fetch"))}
        </SettingsError>
        <SettingsError>Errors: two fields are empty.</SettingsError>
      </>,
    );

    expect(
      screen.getAllByRole("alert").map((line) => line.textContent),
    ).toEqual([
      "Could not reach the gateway.",
      "409: That name is taken.",
      "Failed to fetch",
      "Errors: two fields are empty.",
    ]);
  });

  it("prints in critical ink, which reads as text on the page", () => {
    // The tint's rung, text-critical-foreground, went near-white on the dark
    // page.
    render(<SettingsError>Could not save.</SettingsError>);

    expect(screen.getByRole("alert")).toHaveClass("text-critical");
    expect(screen.getByRole("alert")).not.toHaveClass(
      "text-critical-foreground",
    );
  });
});

describe("SettingsStatus", () => {
  it.each<[SettingsStatusTone, string | null]>([
    ["ready", "notice-success"],
    ["neutral", null],
    ["warning", "notice-warning"],
    ["critical", "notice-critical"],
  ])("draws the %s tone on the notice edge", (tone, notice) => {
    render(<SettingsStatus tone={tone} label="Verdict" description="Why." />);

    const tones = [...screen.getByRole("status").classList].filter((name) =>
      name.startsWith("notice-"),
    );
    expect(tones).toEqual(notice ? [notice] : []);
  });
});
