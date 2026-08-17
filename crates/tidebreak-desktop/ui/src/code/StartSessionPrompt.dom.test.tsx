// @vitest-environment jsdom
import { cleanup, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { HarnessDoctorEntry } from "../api/types";
import { renderWithRouter } from "@/test/router";
import { UNSUPERVISED_AUTO_NOTE } from "./labels";
import { StartSessionPrompt } from "./StartSessionPrompt";

afterEach(() => {
  cleanup();
});

const CAPS = {
  resume: "supported",
  streaming_deltas: "supported",
  mid_turn_steering: "unsupported",
  plan_mode: "supported",
  auto_mode: "supported",
  reasoning_levels: "unknown",
  native_file_change_events: "unsupported",
  native_interrupt: "supported",
} as const;

function entry(
  kind: HarnessDoctorEntry["kind"],
  caps: Partial<HarnessDoctorEntry["caps"]>,
): HarnessDoctorEntry {
  return {
    kind,
    found: true,
    tier: "reference",
    caps: { ...CAPS, structured_approvals: "supported", ...caps },
    remediation: "",
    stderr: "",
    unrecognized_event_count: 0,
  };
}

describe("StartSessionPrompt", () => {
  it("defaults to Ask and posts Ask when the doctor supports structured approvals", async () => {
    const user = userEvent.setup();
    const onStart = vi.fn();
    await renderWithRouter(
      <StartSessionPrompt
        harnesses={[entry("claude_code", {})]}
        starting={false}
        selectedMode={null}
        onSelectMode={vi.fn()}
        onStart={onStart}
      />,
    );
    expect(screen.getByRole("button", { name: "Ask" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    await user.click(screen.getByRole("button", { name: "Start session" }));
    expect(onStart).toHaveBeenCalledWith("claude_code", "ask");
  });

  it("falls back to Plan when structured approvals are not supported", async () => {
    const user = userEvent.setup();
    const onStart = vi.fn();
    await renderWithRouter(
      <StartSessionPrompt
        harnesses={[
          entry("claude_code", {
            structured_approvals: "unsupported",
            auto_mode: "unsupported",
          }),
        ]}
        starting={false}
        selectedMode={null}
        onSelectMode={vi.fn()}
        onStart={onStart}
      />,
    );
    expect(screen.getByRole("button", { name: /Ask/ })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Plan" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    await user.click(screen.getByRole("button", { name: "Start session" }));
    expect(onStart).toHaveBeenCalledWith("claude_code", "plan");
  });

  it("switches an Auto-only engine to unsupervised Auto and says so", async () => {
    const user = userEvent.setup();
    const onStart = vi.fn();
    await renderWithRouter(
      <StartSessionPrompt
        harnesses={[
          entry("claude_code", {}),
          entry("grok", {
            plan_mode: "unsupported",
            structured_approvals: "unsupported",
          }),
        ]}
        starting={false}
        selectedMode="ask"
        onSelectMode={vi.fn()}
        onStart={onStart}
      />,
    );
    expect(screen.queryByText(UNSUPERVISED_AUTO_NOTE)).toBeNull();
    await user.click(screen.getByRole("combobox", { name: "Harness" }));
    await user.click(screen.getByRole("option", { name: /Grok CLI/ }));
    // The selected Ask is not honorable here; the mode follows the engine.
    expect(screen.getByRole("button", { name: "Auto" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    expect(screen.getByText(UNSUPERVISED_AUTO_NOTE)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Start session" }));
    expect(onStart).toHaveBeenCalledWith("grok", "auto");
  });
});
