// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { HarnessDoctorEntry } from "../api/types";
import { EngineSignInDialog, type EngineSignInClient } from "./EngineSignIn";

const written: string[] = [];

vi.mock("@xterm/xterm", () => ({
  Terminal: class MockTerminal {
    cols = 100;
    rows = 30;
    options = {};
    write(data: string, cb?: () => void) {
      written.push(data);
      cb?.();
    }
    loadAddon() {}
    open() {}
    focus() {}
    dispose() {}
    onData() {
      return { dispose() {} };
    }
  },
}));

vi.mock("@xterm/addon-fit", () => ({
  FitAddon: class {
    fit() {}
  },
}));

vi.mock("@xterm/xterm/css/xterm.css", () => ({}));

afterEach(() => {
  cleanup();
  written.length = 0;
});

function encode(text: string): string {
  const bytes = new TextEncoder().encode(text);
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary);
}

const terminal = {
  id: "sign-in-1",
  kind: "claude_code" as const,
  command: "claude auth login",
  cols: 100,
  rows: 30,
  ended: false,
  created_at: "2026-09-23T12:00:00.000Z",
};

function page(bytes: string, ended: boolean) {
  return {
    id: terminal.id,
    kind: terminal.kind,
    bytes: encode(bytes),
    cursor: bytes.length,
    overflow: false,
    truncated: false,
    ended,
  };
}

function client(
  overrides: Partial<EngineSignInClient> = {},
): EngineSignInClient {
  return {
    startHarnessSignIn: vi.fn().mockResolvedValue(terminal),
    readHarnessSignIn: vi.fn().mockResolvedValue(page("", false)),
    writeHarnessSignIn: vi.fn().mockResolvedValue(undefined),
    resizeHarnessSignIn: vi.fn().mockResolvedValue(terminal),
    closeHarnessSignIn: vi.fn().mockResolvedValue(undefined),
    ...overrides,
  };
}

function entry(authenticated: boolean | undefined): HarnessDoctorEntry {
  return {
    kind: "claude_code",
    found: true,
    installable: true,
    authenticated,
    auth_mode: "local_sign_in",
    tier: "reference",
    caps: {
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
      mid_turn_resume: "unsupported",
      transcript: "unsupported",
      memory_loopback: "unsupported",
    },
    commands: [],
    remediation: "",
    stderr: "",
    unrecognized_event_count: 0,
    relaunch_composes_permission_mode: true,
    update_available: false,
    sign_in_command: "claude auth login",
  };
}

describe("EngineSignInDialog", () => {
  it("runs the engine's sign-in and checks it again when the command exits", async () => {
    const signIn = client({
      readHarnessSignIn: vi
        .fn()
        .mockResolvedValue(page("Login successful.\r\n", true)),
    });
    let current = entry(false);
    const onRecheck = vi.fn(async () => {
      current = entry(true);
    });
    const { rerender } = render(
      <EngineSignInDialog
        client={signIn}
        kind="claude_code"
        entry={current}
        open
        onOpenChange={vi.fn()}
        onRecheck={onRecheck}
      />,
    );

    expect(screen.getByText("claude auth login").tagName).toBe("CODE");
    await waitFor(() =>
      expect(signIn.startHarnessSignIn).toHaveBeenCalledWith("claude_code", {
        cols: 100,
        rows: 30,
      }),
    );
    await waitFor(() => expect(onRecheck).toHaveBeenCalledTimes(1));
    await waitFor(() =>
      expect(written.join("")).toContain("Login successful."),
    );
    rerender(
      <EngineSignInDialog
        client={signIn}
        kind="claude_code"
        entry={current}
        open
        onOpenChange={vi.fn()}
        onRecheck={onRecheck}
      />,
    );
    expect(
      await screen.findByText("Claude Code is signed in."),
    ).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Run it again" })).toBeNull();
  });

  it("offers another run when the engine is still signed out", async () => {
    const signIn = client({
      readHarnessSignIn: vi
        .fn()
        .mockResolvedValue(page("Cancelled.\r\n", true)),
    });
    render(
      <EngineSignInDialog
        client={signIn}
        kind="claude_code"
        entry={entry(false)}
        open
        onOpenChange={vi.fn()}
        onRecheck={vi.fn(async () => {})}
      />,
    );

    expect(
      await screen.findByText("Claude Code is still signed out."),
    ).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Run it again" }));
    await waitFor(() =>
      expect(signIn.startHarnessSignIn).toHaveBeenCalledTimes(2),
    );
  });

  // Cancel stops the login rather than leaving it waiting on a prompt
  // nobody can see, and still checks: the credentials may already be saved.
  it("stops a running sign-in on Cancel and checks anyway", async () => {
    const signIn = client();
    const onRecheck = vi.fn(async () => {});
    const onOpenChange = vi.fn();
    render(
      <EngineSignInDialog
        client={signIn}
        kind="claude_code"
        entry={entry(false)}
        open
        onOpenChange={onOpenChange}
        onRecheck={onRecheck}
      />,
    );

    await waitFor(() => expect(signIn.readHarnessSignIn).toHaveBeenCalled());
    await userEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(signIn.closeHarnessSignIn).toHaveBeenCalledWith(
      "claude_code",
      "sign-in-1",
    );
    expect(onRecheck).toHaveBeenCalledTimes(1);
    expect(onOpenChange).toHaveBeenCalledWith(false);
  });
});
