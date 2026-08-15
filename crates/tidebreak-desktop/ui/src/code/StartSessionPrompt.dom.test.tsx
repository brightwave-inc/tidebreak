// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { HarnessDoctorEntry } from "../api/types";
import { StartSessionPrompt } from "./StartSessionPrompt";

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
} as const;

function entry(
  kind: HarnessDoctorEntry["kind"],
  structuredApprovals: HarnessDoctorEntry["caps"]["structured_approvals"],
): HarnessDoctorEntry {
  return {
    kind,
    found: true,
    tier: "reference",
    caps: { ...CAPS, structured_approvals: structuredApprovals },
    remediation: "",
    stderr: "",
    unrecognized_event_count: 0,
  };
}

describe("StartSessionPrompt", () => {
  it("defaults to Ask and posts Ask when the doctor supports structured approvals", () => {
    const onStart = vi.fn();
    render(
      <StartSessionPrompt
        harnesses={[entry("claude_code", "supported")]}
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
    fireEvent.click(screen.getByRole("button", { name: "Claude Code" }));
    expect(onStart).toHaveBeenCalledWith("claude_code", "ask");
  });

  it("falls back to Plan when structured approvals are not supported", () => {
    const onStart = vi.fn();
    render(
      <StartSessionPrompt
        harnesses={[entry("claude_code", "unsupported")]}
        starting={false}
        selectedMode={null}
        onSelectMode={vi.fn()}
        onStart={onStart}
      />,
    );
    const ask = screen.getByRole("button", { name: /Ask/ });
    expect(ask).toBeDisabled();
    expect(screen.getByRole("button", { name: "Plan" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    fireEvent.click(screen.getByRole("button", { name: "Claude Code" }));
    expect(onStart).toHaveBeenCalledWith("claude_code", "plan");
  });

  it("posts Plan for a harness that cannot honor a selected Ask mode", () => {
    const onStart = vi.fn();
    render(
      <StartSessionPrompt
        harnesses={[
          entry("claude_code", "supported"),
          entry("codex", "unsupported"),
        ]}
        starting={false}
        selectedMode="ask"
        onSelectMode={vi.fn()}
        onStart={onStart}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Codex CLI" }));
    expect(onStart).toHaveBeenCalledWith("codex", "plan");
  });
});
