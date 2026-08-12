// @vitest-environment jsdom
import { cleanup, render, screen, within } from "@testing-library/react";
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
      detail: {
        kind: "exec",
        command: "pip",
        args: ["install", "matplotlib"],
        exit_code: 1,
        output: "exit: 1\n\nstderr:\nERROR: no matching distribution",
      },
    },
    {
      kind: "web_search",
      outcome: "waiting",
      at: "2026-08-05T18:38:00Z",
    },
  ],
};

describe("AgentActivityTimeline", () => {
  it("shows the latest live activity without presenting waiting as running", () => {
    const { rerender } = render(
      <AgentActivityTimeline
        state={state}
        active
        activeLabel="Waiting to search the web"
      />,
    );

    const liveTrigger = screen.getByRole("button", {
      name: "Waiting to search the web",
    });
    expect(liveTrigger.getAttribute("aria-expanded")).toBe("true");
    expect(liveTrigger.querySelector(".animate-pulse")).toBeNull();

    const waitingRow = within(screen.getByRole("list"))
      .getByText("Waiting to search the web")
      .closest("li");
    expect(waitingRow?.querySelector(".lucide-clock")).toBeTruthy();
    expect(waitingRow?.querySelector(".animate-spin")).toBeNull();
    expect(waitingRow?.querySelector(".animate-pulse")).toBeNull();

    rerender(<AgentActivityTimeline state={state} active={false} />);
    const settledTrigger = screen.getByRole("button", {
      name: "Ran 3 tool calls · 1 failed",
    });
    expect(settledTrigger.getAttribute("aria-expanded")).toBe("false");
    expect(screen.queryByRole("list")).toBeNull();
  });

  it("gives a failed command an open card with its headline, exit status, and captured output", () => {
    const { rerender } = render(
      <AgentActivityTimeline state={state} active={false} expanded />,
    );

    const commandRow = within(screen.getByRole("list")).getAllByRole(
      "listitem",
    )[1]!;
    const card = within(commandRow).getByRole("button", {
      name: /pip install matplotlib/,
    });
    expect(card.getAttribute("aria-expanded")).toBe("true");
    expect(card.querySelector(".font-mono")?.textContent).toBe(
      "pip install matplotlib",
    );
    expect(within(commandRow).getByText("Exit 1")).toBeTruthy();
    expect(
      within(commandRow).getByText(/ERROR: no matching distribution/),
    ).toBeTruthy();

    // A settled command that printed nothing says so, rather than leaving the
    // reader to wonder whether the pane failed to load.
    const withoutOutput: AgentActivityState = {
      ...state,
      items: state.items.map((item, index) =>
        index === 1
          ? {
              ...item,
              detail: { kind: "exec", command: "pip", args: [], exit_code: 0 },
            }
          : item,
      ),
    };
    rerender(
      <AgentActivityTimeline state={withoutOutput} active={false} expanded />,
    );
    expect(screen.getByText("No output captured.")).toBeTruthy();
  });
});
