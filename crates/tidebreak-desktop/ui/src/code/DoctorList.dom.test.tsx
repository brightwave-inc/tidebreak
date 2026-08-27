// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { HarnessDoctorEntry, HarnessDoctorReport } from "../api/types";
import { DoctorList } from "./DoctorList";

afterEach(cleanup);

describe("DoctorList", () => {
  it("labels authenticated, signed-out, and unverified engines distinctly", () => {
    render(
      <DoctorList
        report={{
          harnesses: [
            {
              ...notDownloaded,
              kind: "claude_code",
              found: true,
              authenticated: true,
            },
            {
              ...notDownloaded,
              kind: "codex",
              found: true,
              authenticated: false,
              remediation: "Sign in to Codex CLI, then re-check.",
            },
            {
              ...notDownloaded,
              kind: "opencode",
              found: true,
              remediation: "Sign in via your terminal, then re-check.",
            },
          ],
        }}
      />,
    );

    expect(screen.getByText("Ready")).toBeInTheDocument();
    expect(screen.getByText("Signed out")).toBeInTheDocument();
    expect(screen.getByText("Unverified")).toBeInTheDocument();
    expect(
      screen.getByText("Sign in via your terminal, then re-check."),
    ).toBeInTheDocument();
    expect(screen.getByText("1 of 3 engines ready.")).toBeInTheDocument();
  });

  it("directs an all-installed blocked set to sign in instead of download", () => {
    render(
      <DoctorList
        report={{
          harnesses: [
            {
              ...notDownloaded,
              kind: "claude_code",
              found: true,
              remediation: "Sign in via your terminal, then re-check.",
            },
            {
              ...notDownloaded,
              kind: "codex",
              found: true,
              authenticated: false,
            },
          ],
        }}
      />,
    );

    expect(
      screen.getByText(
        "No engine is ready yet. Sign in to one below, then re-check.",
      ),
    ).toBeInTheDocument();
  });

  it("states that protocol-gap counts include saved session history", async () => {
    const report: HarnessDoctorReport = {
      harnesses: [
        {
          kind: "codex",
          found: true,
          installable: true,
          version: "codex-cli 0.147.0",
          tier: "secondary",
          caps: {
            resume: "supported",
            streaming_deltas: "supported",
            mid_turn_steering: "unknown",
            plan_mode: "supported",
            auto_mode: "supported",
            allow_mode: "supported",
            reasoning_levels: "supported",
            native_file_change_events: "unknown",
            native_interrupt: "supported",
            structured_approvals: "supported",
            image_input: "unknown",
            slash_commands: "unknown",
          },
          commands: [],
          auth_mode: "local_sign_in",
          remediation: "",
          stderr: "",
          unrecognized_event_count: 6,
          relaunch_composes_permission_mode: true,
        },
      ],
    };

    render(<DoctorList report={report} />);

    // Diagnostics sit behind the row's disclosure; the resting row is the
    // engine, its state, and the one thing to do about it.
    await userEvent
      .setup()
      .click(screen.getByRole("button", { name: "Details for Codex CLI" }));
    expect(screen.getByText("Protocol gaps (history):")).toBeInTheDocument();
    expect(
      screen.getByText("6 events across saved sessions"),
    ).toBeInTheDocument();
  });

  // The point of the lazy pin: a missing engine offers its own download
  // instead of sending the reader off to install every engine at once.
  it("offers a download for an engine this machine has not fetched", async () => {
    const onInstall = vi.fn();
    render(
      <DoctorList
        report={{ harnesses: [notDownloaded] }}
        onInstall={onInstall}
      />,
    );

    expect(screen.getByText("Not downloaded")).toBeInTheDocument();
    expect(
      screen.getByText("Downloads the first time you pick it."),
    ).toBeInTheDocument();
    await userEvent
      .setup()
      .click(screen.getByRole("button", { name: /Download/ }));
    expect(onInstall).toHaveBeenCalledWith("opencode");
  });

  it("reads relay-covered engines as ready on a hosted machine", () => {
    render(
      <DoctorList
        report={{
          harnesses: [
            {
              ...notDownloaded,
              kind: "claude_code",
              found: true,
              // The hosted machine's own probe sees no sign-in, and that is
              // no longer the verdict: the relay carries the turn.
              authenticated: false,
              auth_mode: "gateway_relay",
            },
            {
              ...notDownloaded,
              kind: "opencode",
              found: true,
              authenticated: false,
              auth_mode: "hosted_unavailable",
              remediation: "opencode is not available on hosted machines yet.",
            },
          ],
        }}
      />,
    );

    expect(screen.getByText("Ready")).toBeInTheDocument();
    expect(
      screen.getByText("Turns run as you through the Model Gateway."),
    ).toBeInTheDocument();
    expect(screen.getByText("Unavailable")).toBeInTheDocument();
    expect(
      screen.getByText("opencode is not available on hosted machines yet."),
    ).toBeInTheDocument();
    expect(screen.getByText("1 of 2 engines ready.")).toBeInTheDocument();
    expect(
      screen.queryByText(
        "No engine is ready yet. Sign in to one below, then re-check.",
      ),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByText("Sign in via your terminal, then re-check."),
    ).not.toBeInTheDocument();
  });
  it("names a gateway-managed engine instead of demanding a sign-in", async () => {
    render(
      <DoctorList
        report={{
          harnesses: [
            {
              ...notDownloaded,
              kind: "claude_code",
              found: true,
              // A machine pointed at the gateway has no vendor login, and
              // the engine's own check says so (issue 2749).
              authenticated: false,
              auth_mode: "gateway_managed",
            },
          ],
        }}
      />,
    );

    expect(screen.getByText("Gateway-managed")).toBeInTheDocument();
    expect(
      screen.getByText("Credentials are managed for this machine."),
    ).toBeInTheDocument();
    expect(screen.getByText("1 of 1 engine ready.")).toBeInTheDocument();
    expect(
      screen.queryByText("Sign in via your terminal, then re-check."),
    ).not.toBeInTheDocument();
    expect(screen.queryByText("Signed out")).toBeNull();

    await userEvent
      .setup()
      .click(screen.getByRole("button", { name: "Details for Claude Code" }));
    expect(
      screen.getByText("not needed — credentials are managed here"),
    ).toBeInTheDocument();
  });

  /** An engine still downloading offers no second Download button. */
  it("reports a download in flight instead of offering another", () => {
    render(
      <DoctorList
        report={{ harnesses: [notDownloaded] }}
        onInstall={vi.fn()}
        installs={{
          opencode: {
            kind: "opencode",
            version: "1.18.18",
            phase: "installing",
            done: false,
          },
        }}
      />,
    );

    expect(screen.getByText("Downloading")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Download/ })).toBeNull();
  });
});

const notDownloaded: HarnessDoctorEntry = {
  kind: "opencode",
  found: false,
  installable: true,
  tier: "tertiary",
  caps: {
    resume: "supported",
    streaming_deltas: "supported",
    mid_turn_steering: "unknown",
    plan_mode: "supported",
    auto_mode: "supported",
    allow_mode: "supported",
    reasoning_levels: "supported",
    native_file_change_events: "unknown",
    native_interrupt: "supported",
    structured_approvals: "supported",
    image_input: "unknown",
    slash_commands: "unknown",
  },
  commands: [],
  auth_mode: "local_sign_in",
  remediation: "",
  stderr: "",
  unrecognized_event_count: 0,
  relaunch_composes_permission_mode: false,
};
