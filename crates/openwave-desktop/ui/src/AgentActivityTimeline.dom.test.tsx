// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import {
  AgentActivityTimeline,
  type AgentActivityState,
} from "./AgentActivityTimeline";

afterEach(cleanup);

const state: AgentActivityState = {
  loading: false,
  error: false,
  loaded: true,
  items: [
    {
      kind: "read_delegated_file",
      outcome: "completed",
      at: "2026-08-05T18:36:00Z",
    },
    {
      kind: "exec",
      outcome: "failed",
      at: "2026-08-05T18:37:00Z",
    },
    {
      kind: "web_search",
      outcome: "completed",
      at: "2026-08-05T18:38:00Z",
    },
  ],
};

describe("AgentActivityTimeline", () => {
  it("follows the run by default and preserves an explicit user toggle", () => {
    const { rerender } = render(
      <AgentActivityTimeline state={state} active />,
    );

    const summary = screen.getByRole("button", {
      name: "Ran 3 tool calls · 1 failed",
    });
    expect(summary.getAttribute("aria-expanded")).toBe("true");
    expect(screen.getByRole("list")).toBeTruthy();

    rerender(<AgentActivityTimeline state={state} active={false} />);
    expect(summary.getAttribute("aria-expanded")).toBe("false");
    expect(screen.queryByRole("list")).toBeNull();

    fireEvent.click(summary);
    expect(summary.getAttribute("aria-expanded")).toBe("true");
    expect(screen.getByRole("list")).toBeTruthy();

    rerender(<AgentActivityTimeline state={state} active />);
    rerender(<AgentActivityTimeline state={state} active={false} />);
    expect(summary.getAttribute("aria-expanded")).toBe("true");
    expect(screen.getByRole("list")).toBeTruthy();
  });
});
