// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { HarnessDoctorEntry, HarnessDoctorReport } from "../api/types";
import { DoctorList } from "./DoctorList";

afterEach(cleanup);

describe("DoctorList", () => {
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
          remediation: "",
          stderr: "",
          unrecognized_event_count: 6,
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
  remediation: "",
  stderr: "",
  unrecognized_event_count: 0,
};
