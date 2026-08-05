// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";

import type { AgentRun } from "./api";
import { ChatStatusChip } from "./ChatStatusChip";
import type { ChatFolderAccess } from "./useChatFolderAttachments";

const folder = {
  rootId: "root-1",
  displayName: "Notes",
  status: "connected",
  statements: [],
} as unknown as ChatFolderAccess;

function run(id: string, status: AgentRun["status"]): AgentRun {
  return {
    id,
    parent_id: null,
    tier: "background",
    status,
    spawn_call_id: `call-${id}`,
  } as unknown as AgentRun;
}

function renderChip({
  outputCount = 0,
  runs = [] as AgentRun[],
} = {}) {
  const onOpenOutputs = vi.fn();
  const onOpenFolders = vi.fn();
  const onOpenAgents = vi.fn();
  render(
    <ChatStatusChip
      outputCount={outputCount}
      folders={[folder]}
      runs={runs}
      onOpenOutputs={onOpenOutputs}
      onOpenFolders={onOpenFolders}
      onOpenAgents={onOpenAgents}
    />,
  );
  return { onOpenOutputs, onOpenFolders, onOpenAgents };
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

it("summarizes activity on its face and opens the chat-scoped places", async () => {
  const { onOpenOutputs, onOpenFolders } = renderChip({ outputCount: 2 });

  // No live work, so the face falls back to what the chat has produced.
  const chip = screen.getByRole("button", { name: "Chat activity: 2 outputs" });

  await userEvent.click(chip);
  // The label span sits inside the row button, so the click lands on it.
  await userEvent.click(await screen.findByText("Outputs"));
  expect(onOpenOutputs).toHaveBeenCalled();

  await userEvent.click(chip);
  await userEvent.click(await screen.findByText("Folders"));
  expect(onOpenFolders).toHaveBeenCalled();
});

/**
 * The dots are the whole reason the chip is persistent: background work is
 * invisible from a scrolled-away transcript otherwise.
 */
it("counts live background runs and opens the agents table", async () => {
  const { onOpenAgents } = renderChip({
    runs: [run("run-1", "running"), run("run-2", "retry_wait"), run("run-3", "completed")],
  });

  const chip = screen.getByRole("button", { name: "Chat activity: 2 running" });
  await userEvent.click(chip);
  await userEvent.click(screen.getByText("2 of 3 running"));
  expect(onOpenAgents).toHaveBeenCalled();
});
