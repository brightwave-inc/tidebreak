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
  durable_parks: "unsupported",
  user_questions: "unsupported",
  standing_grants: "unsupported",
  memory_loopback: "unsupported",
} as const;

function entry(
  overrides: Partial<HarnessDoctorEntry> & Pick<HarnessDoctorEntry, "kind">,
): HarnessDoctorEntry {
  return {
    found: true,
    installable: true,
    version: "9.9.9",
    path: "/opt/harness",
    tier: "reference",
    caps: { ...CAPS },
    commands: [],
    auth_mode: "local_sign_in",
    remediation: "",
    stderr: "",
    unrecognized_event_count: 0,
    relaunch_composes_permission_mode: true,
    update_available: false,
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
          entry({ kind: "claude_code", authenticated: true }),
          entry({ kind: "codex", authenticated: true }),
          entry({ kind: "opencode", found: false, installable: false }),
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

  // The lazy pin: an engine this machine has never fetched is a wait, not a
  // fault, so it stays selectable and picking it is what starts the download.
  it("offers an engine that has not been downloaded yet", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    await renderWithRouter(
      <HarnessPicker
        harnesses={[
          entry({ kind: "claude_code", authenticated: true }),
          entry({
            kind: "opencode",
            found: false,
            version: undefined,
            path: undefined,
          }),
        ]}
        value="claude_code"
        onChange={onChange}
      />,
    );
    await user.click(screen.getByRole("combobox", { name: "Harness" }));
    const row = screen.getByRole("option", { name: /opencode/ });
    expect(row).toHaveTextContent("Downloads on first use");
    expect(row).not.toHaveAttribute("aria-disabled", "true");
    await user.click(row);
    expect(onChange).toHaveBeenCalledWith("opencode");
  });

  it("keeps an Auto-only engine selectable and never renders doctor strings", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    await renderWithRouter(
      <HarnessPicker
        harnesses={[
          entry({
            kind: "claude_code",
            version: "2.1.233",
            authenticated: true,
          }),
          entry({
            kind: "grok",
            version: "1.0.4",
            authenticated: true,
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

  it("disables an engine whose sign-in status is unverified", async () => {
    const onChange = vi.fn();
    await renderWithRouter(
      <HarnessPicker
        harnesses={[
          entry({
            kind: "claude_code",
            authenticated: undefined,
            remediation: "Sign in via your terminal, then re-check.",
          }),
          entry({ kind: "codex", authenticated: true }),
        ]}
        value="codex"
        onChange={onChange}
      />,
    );

    await userEvent
      .setup()
      .click(screen.getByRole("combobox", { name: "Harness" }));
    const row = screen.getByRole("option", { name: /Claude Code/ });
    expect(row).toHaveTextContent("Unverified — sign in via your terminal");
    expect(row).toHaveAttribute("aria-disabled", "true");
    expect(onChange).not.toHaveBeenCalled();
  });

  it("does not add a settings button beside the composer control", async () => {
    await renderWithRouter(
      <HarnessPicker
        harnesses={[
          entry({ kind: "claude_code", authenticated: true }),
          entry({ kind: "opencode", found: false, installable: false }),
        ]}
        value="claude_code"
        onChange={vi.fn()}
        variant="composer"
      />,
    );

    expect(
      screen.queryByRole("button", { name: "Coding harnesses" }),
    ).not.toBeInTheDocument();
  });
});
