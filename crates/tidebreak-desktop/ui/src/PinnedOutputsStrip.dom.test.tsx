// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

import type { DeliverableSummary } from "./deliverables";
import { PinnedOutputsStrip, useOutputsStripStore } from "./PinnedOutputsStrip";

function output(
  outputId: string,
  filename: string,
  updatedAt: string,
): DeliverableSummary {
  return {
    outputId,
    filename,
    mediaType: "text/markdown",
    sizeBytes: 12,
    revisionCount: 1,
    updatedAt,
    producingRunId: null,
  };
}

const outputs = [
  output("out-1", "plan.md", "2026-08-01T10:00:00Z"),
  output("out-2", "summary.md", "2026-08-02T10:00:00Z"),
];

function renderStrip({ panelOpen = false } = {}) {
  const onOpenOutput = vi.fn();
  const onOpenOutputs = vi.fn();
  render(
    <PinnedOutputsStrip
      chatId="chat-1"
      outputs={outputs}
      panelOpen={panelOpen}
      onOpenOutput={onOpenOutput}
      onOpenOutputs={onOpenOutputs}
    />,
  );
  return { onOpenOutput, onOpenOutputs };
}

beforeEach(() => {
  window.sessionStorage.clear();
  useOutputsStripStore.setState({ collapsed: {} });
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

it("names the outputs by default and opens the one clicked", async () => {
  const { onOpenOutput } = renderStrip();

  // Newest first — the file the last turn wrote is the one being looked for.
  const chips = screen.getAllByRole("button", { name: /^Open output/ });
  expect(chips.map((chip) => chip.textContent)).toEqual([
    "summary.md",
    "plan.md",
  ]);

  await userEvent.click(chips[1]!);
  expect(onOpenOutput).toHaveBeenCalledWith("out-1");
});

/** The panel supersedes the strip: two copies of the outputs is just chrome. */
it("stays out of the way while a panel is open", () => {
  renderStrip({ panelOpen: true });
  expect(screen.queryByRole("region", { name: "Outputs" })).toBeNull();
});

it("remembers being collapsed across a remount of the chat route", async () => {
  const first = renderStrip();
  await userEvent.click(screen.getByRole("button", { name: "Hide outputs" }));
  expect(screen.queryByRole("button", { name: /^Open output/ })).toBeNull();
  expect(first.onOpenOutputs).not.toHaveBeenCalled();

  cleanup();
  renderStrip();
  expect(screen.queryByRole("button", { name: /^Open output/ })).toBeNull();
  expect(screen.getByRole("button", { name: "Show outputs" })).toBeTruthy();
});
