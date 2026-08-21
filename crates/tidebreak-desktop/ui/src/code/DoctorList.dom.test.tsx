// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import type { HarnessDoctorReport } from "../api/types";
import { DoctorList } from "./DoctorList";

afterEach(cleanup);

describe("DoctorList", () => {
  it("states that protocol-gap counts include saved session history", () => {
    const report: HarnessDoctorReport = {
      harnesses: [
        {
          kind: "codex",
          found: true,
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

    expect(screen.getByText("Protocol gaps (history):")).toBeInTheDocument();
    expect(
      screen.getByText("6 events across saved sessions"),
    ).toBeInTheDocument();
  });
});
