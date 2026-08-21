// @vitest-environment jsdom
import { cleanup, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
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
  auto_mode: "supported",
  allow_mode: "supported",
  reasoning_levels: "unknown",
  native_file_change_events: "unsupported",
  native_interrupt: "supported",
  structured_approvals: "supported",
  image_input: "unknown",
  slash_commands: "unknown",
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
    commands: [],
    remediation: "",
    stderr: "",
    unrecognized_event_count: 0,
    ...overrides,
  };
}

describe("HarnessPicker", () => {
  it("is a dropdown: ready rows select, unusable rows are disabled with a reason", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    await renderWithRouter(
      <HarnessPicker
        harnesses={[
          entry({ kind: "claude_code" }),
          entry({ kind: "codex" }),
          entry({ kind: "opencode", found: false }),
        ]}
        value="claude_code"
        onChange={onChange}
      />,
    );
    const trigger = screen.getByRole("combobox", { name: "Harness" });
    expect(trigger).toHaveTextContent("Claude Code");
    await user.click(trigger);
    // Ready rows carry the product name and nothing else: no vendor gloss.
    expect(
      screen.getByRole("option", { name: "Codex CLI" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("option", { name: /Not installed/ }),
    ).toHaveAttribute("aria-disabled", "true");
    await user.click(screen.getByRole("option", { name: /Codex CLI/ }));
    expect(onChange).toHaveBeenCalledWith("codex");
  });

  it("keeps an Auto-only engine selectable and never renders doctor strings", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    await renderWithRouter(
      <HarnessPicker
        harnesses={[
          entry({ kind: "claude_code", version: "2.1.233" }),
          entry({
            kind: "grok",
            version: "1.0.4",
            caps: {
              ...CAPS,
              plan_mode: "unsupported",
              structured_approvals: "unsupported",
            },
          }),
        ]}
        value="claude_code"
        onChange={onChange}
      />,
    );
    await user.click(screen.getByRole("combobox", { name: "Harness" }));
    const listbox = screen.getByRole("listbox");
    expect(listbox.textContent).not.toMatch(/2\.1\.233|1\.0\.4|\/opt\/harness/);
    const grokRow = screen.getByRole("option", { name: /Grok CLI/ });
    expect(grokRow).not.toHaveAttribute("aria-disabled", "true");
    await user.click(grokRow);
    expect(onChange).toHaveBeenCalledWith("grok");
  });
});
