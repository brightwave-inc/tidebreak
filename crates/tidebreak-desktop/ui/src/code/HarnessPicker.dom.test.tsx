// @vitest-environment jsdom
import { cleanup, fireEvent, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { HarnessDoctorEntry } from "../api/types";
import { renderWithRouter } from "@/test/router";
import { HarnessPicker } from "./HarnessPicker";

afterEach(() => {
  cleanup();
});

const CAPS = {
  resume: "supported",
  streaming_deltas: "supported",
  mid_turn_steering: "unsupported",
  plan_mode: "supported",
  reasoning_levels: "unknown",
  native_file_change_events: "unsupported",
  native_interrupt: "supported",
  structured_approvals: "supported",
} as const;

function entry(
  overrides: Partial<HarnessDoctorEntry> & Pick<HarnessDoctorEntry, "kind">,
): HarnessDoctorEntry {
  return {
    found: true,
    version: "9.9.9",
    path: "/opt/harness",
    tier: "reference",
    caps: { ...CAPS },
    remediation: "",
    stderr: "",
    unrecognized_event_count: 0,
    ...overrides,
  };
}

describe("HarnessPicker", () => {
  it("selects a ready row and dims unusable ones", async () => {
    const onChange = vi.fn();
    await renderWithRouter(
      <HarnessPicker
        harnesses={[
          entry({ kind: "claude_code" }),
          entry({ kind: "codex", found: false }),
          entry({
            kind: "grok",
            caps: { ...CAPS, plan_mode: "unsupported", structured_approvals: "unsupported" },
          }),
        ]}
        value="claude_code"
        onChange={onChange}
      />,
    );
    expect(screen.getByRole("option", { name: /Claude Code/ })).toBeEnabled();
    expect(screen.getByRole("option", { name: /Not installed/ })).toBeDisabled();
    expect(screen.getByRole("option", { name: /Not available yet/ })).toBeDisabled();
    fireEvent.click(screen.getByRole("option", { name: /Claude Code/ }));
    expect(onChange).toHaveBeenCalledWith("claude_code");
  });

  it("moves with the keyboard and never renders doctor version strings", async () => {
    const onChange = vi.fn();
    await renderWithRouter(
      <HarnessPicker
        harnesses={[
          entry({ kind: "claude_code", version: "2.1.233" }),
          entry({ kind: "codex", version: "0.80.0" }),
        ]}
        value="claude_code"
        onChange={onChange}
      />,
    );
    const list = screen.getByRole("listbox", { name: "Harness" });
    expect(list.textContent).not.toMatch(/2\.1\.233|0\.80\.0|\/opt\/harness/);
    fireEvent.keyDown(list, { key: "ArrowDown" });
    fireEvent.keyDown(list, { key: "Enter" });
    expect(onChange).toHaveBeenCalledWith("codex");
  });
});
