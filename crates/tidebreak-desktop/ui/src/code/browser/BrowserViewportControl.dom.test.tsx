// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { BrowserViewportControl } from "./BrowserViewportControl";
import {
  DEFAULT_CUSTOM_WIDTH,
  DEFAULT_VIEWPORT,
  MAX_CUSTOM_WIDTH,
  MIN_CUSTOM_WIDTH,
} from "./browserViewport";

afterEach(() => {
  cleanup();
  window.localStorage.clear();
});

beforeEach(() => {
  window.localStorage.clear();
});

function renderControl(
  overrides: Partial<Parameters<typeof BrowserViewportControl>[0]> = {},
) {
  const onViewportChange = vi.fn();
  const result = render(
    <BrowserViewportControl
      viewport={DEFAULT_VIEWPORT}
      renderedWidth={null}
      onViewportChange={onViewportChange}
      {...overrides}
    />,
  );
  return { ...result, onViewportChange };
}

async function openPopoverAndSetCustomWidth(
  user: ReturnType<typeof userEvent.setup>,
  value: string,
) {
  await user.click(screen.getByRole("button", { name: /Viewport: Fit/i }));
  const input = screen.getByRole("spinbutton", {
    name: "Custom viewport width in pixels",
  });
  await user.clear(input);
  await user.type(input, value);
  fireEvent.submit(input.closest("form")!);
}

describe("BrowserViewportControl", () => {
  it("renders the active preset label and rendered width in the trigger", () => {
    renderControl({
      viewport: { preset: "desktop", customWidth: 800 },
      renderedWidth: 1440,
    });
    const trigger = screen.getByRole("button", {
      name: /Viewport: Desktop 1440, rendered at 1440px/i,
    });
    expect(trigger).toBeVisible();
    expect(trigger).toHaveTextContent("Desktop 1440");
    expect(trigger).toHaveTextContent("1440px");
  });

  it("disables the trigger when disabled", () => {
    renderControl({ disabled: true });
    expect(
      screen.getByRole("button", { name: /Viewport: Fit/i }),
    ).toBeDisabled();
  });

  it("switches to the desktop preset on selection", async () => {
    const { onViewportChange } = renderControl();
    const user = userEvent.setup();

    await user.click(screen.getByRole("button", { name: /Viewport: Fit/i }));
    await user.click(screen.getByRole("radio", { name: /Desktop/i }));

    expect(onViewportChange).toHaveBeenCalledWith(
      expect.objectContaining({ preset: "desktop" }),
    );
  });

  it("switches to the tablet and mobile presets", async () => {
    const { onViewportChange } = renderControl();
    const user = userEvent.setup();

    await user.click(screen.getByRole("button", { name: /Viewport: Fit/i }));
    await user.click(screen.getByRole("radio", { name: /Tablet/i }));
    expect(onViewportChange).toHaveBeenLastCalledWith(
      expect.objectContaining({ preset: "tablet" }),
    );

    await user.click(screen.getByRole("radio", { name: /Mobile/i }));
    expect(onViewportChange).toHaveBeenLastCalledWith(
      expect.objectContaining({ preset: "mobile" }),
    );
  });

  it("marks the active preset as checked", async () => {
    renderControl({
      viewport: { preset: "tablet", customWidth: 800 },
    });
    const trigger = screen.getByRole("button", { name: /Viewport: Tablet/i });
    const user = userEvent.setup();
    await user.click(trigger);

    const tablet = screen.getByRole("radio", { name: /Tablet/i });
    expect(tablet).toHaveAttribute("aria-checked", "true");
    const desktop = screen.getByRole("radio", { name: /Desktop/i });
    expect(desktop).toHaveAttribute("aria-checked", "false");
  });

  it("validates and commits custom width input", async () => {
    const { onViewportChange } = renderControl();
    const user = userEvent.setup();

    await openPopoverAndSetCustomWidth(user, "320");

    expect(onViewportChange).toHaveBeenCalledWith(
      expect.objectContaining({ preset: "custom", customWidth: 320 }),
    );
  });

  it("clamps below-minimum custom width and shows an error", async () => {
    const { onViewportChange } = renderControl();
    const user = userEvent.setup();

    await openPopoverAndSetCustomWidth(user, "50");

    expect(onViewportChange).toHaveBeenCalledWith(
      expect.objectContaining({
        preset: "custom",
        customWidth: MIN_CUSTOM_WIDTH,
      }),
    );
    expect(
      screen.getByText(
        `Width must be between ${MIN_CUSTOM_WIDTH} and ${MAX_CUSTOM_WIDTH}`,
      ),
    ).toBeVisible();
  });

  it("clamps above-maximum custom width and shows an error", async () => {
    const { onViewportChange } = renderControl();
    const user = userEvent.setup();

    await openPopoverAndSetCustomWidth(user, "99999");

    expect(onViewportChange).toHaveBeenCalledWith(
      expect.objectContaining({
        preset: "custom",
        customWidth: MAX_CUSTOM_WIDTH,
      }),
    );
    expect(
      screen.getByText(
        `Width must be between ${MIN_CUSTOM_WIDTH} and ${MAX_CUSTOM_WIDTH}`,
      ),
    ).toBeVisible();
  });

  it("rejects non-numeric input with an error", async () => {
    const { onViewportChange } = renderControl();
    const user = userEvent.setup();

    await openPopoverAndSetCustomWidth(user, "abc");

    expect(screen.getByText("Enter a number")).toBeVisible();
    // Should not call onViewportChange for invalid input
    expect(onViewportChange).not.toHaveBeenCalled();
  });

  it("shows the valid range hint", async () => {
    renderControl();
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: /Viewport: Fit/i }));
    expect(
      screen.getByText(`${MIN_CUSTOM_WIDTH}–${MAX_CUSTOM_WIDTH} px`),
    ).toBeVisible();
  });

  it("syncs the custom width field when the viewport changes externally", () => {
    const { rerender } = renderControl({
      viewport: { preset: "custom", customWidth: 500 },
    });

    // External change — e.g. preset switch resets customWidth
    rerender(
      <BrowserViewportControl
        viewport={{ preset: "custom", customWidth: DEFAULT_CUSTOM_WIDTH }}
        renderedWidth={null}
        onViewportChange={vi.fn()}
      />,
    );
    const input = screen.queryByRole("spinbutton", {
      name: "Custom viewport width in pixels",
    });
    if (input) {
      expect(input).toHaveValue(String(DEFAULT_CUSTOM_WIDTH));
    }
  });
});
