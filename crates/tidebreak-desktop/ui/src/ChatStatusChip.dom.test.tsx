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
  compact = false,
} = {}) {
  const onOpenOutputs = vi.fn();
  const onOpenFolders = vi.fn();
  const onOpenAgents = vi.fn();
  const onOpenPermissions = vi.fn();
  render(
    <ChatStatusChip
      compact={compact}
      outputCount={outputCount}
      folders={[folder]}
      runs={runs}
      onOpenOutputs={onOpenOutputs}
      onOpenFolders={onOpenFolders}
      onOpenAgents={onOpenAgents}
      onOpenPermissions={onOpenPermissions}
    />,
  );
  return { onOpenOutputs, onOpenFolders, onOpenAgents, onOpenPermissions };
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

it("summarizes activity on its face and opens the chat-scoped places", async () => {
  const { onOpenOutputs, onOpenFolders, onOpenPermissions } = renderChip({
    outputCount: 2,
  });

  // With the whole canvas available, the useful places are visible without a
  // disclosure click and the summary falls back to what the chat produced.
  expect(screen.getByLabelText("Work activity")).toHaveTextContent("2 outputs");
  await userEvent.click(await screen.findByText("Outputs"));
  expect(onOpenOutputs).toHaveBeenCalled();

  await userEvent.click(screen.getByText("Folders"));
  expect(onOpenFolders).toHaveBeenCalled();

  await userEvent.click(screen.getByText("Permissions"));
  expect(onOpenPermissions).toHaveBeenCalled();
});

it("folds the open card down to an icon and restores it", async () => {
  renderChip({ outputCount: 2 });

  await userEvent.click(
    screen.getByRole("button", { name: "Collapse work activity" }),
  );
  expect(screen.queryByLabelText("Work activity")).not.toBeInTheDocument();

  await userEvent.click(
    screen.getByRole("button", { name: "Expand work activity" }),
  );
  expect(screen.getByLabelText("Work activity")).toHaveTextContent("2 outputs");
});

/**
 * The dots are the whole reason the chip is persistent: background work is
 * invisible from a scrolled-away transcript otherwise.
 */
it("counts live background runs and opens the agents table", async () => {
  const { onOpenAgents } = renderChip({
    runs: [
      run("run-1", "running"),
      run("run-2", "retry_wait"),
      run("run-3", "completed"),
    ],
    compact: true,
  });

  const chip = screen.getByRole("button", { name: "Work activity: 2 running" });
  await userEvent.click(chip);
  await userEvent.click(screen.getByText("2 of 3 running"));
  expect(onOpenAgents).toHaveBeenCalled();
});
