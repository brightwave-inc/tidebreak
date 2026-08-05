// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

import type { AgentRun, ApiClient, Chat, ModelInfo } from "./api";
import { ChatStatusChip } from "./ChatStatusChip";
import { useChatSessionStore } from "./ChatSessionStore";
import type { ChatFolderAccess } from "./useChatFolderAttachments";

const chat = {
  id: "chat-1",
  title: "Roadmap",
  project_id: null,
  model: "sonnet",
  permission_mode: "auto",
} as unknown as Chat;

const models = [
  { id: "sonnet", display_name: "Claude Sonnet", provider: "anthropic", available: true },
] as unknown as ModelInfo[];

const folder = {
  rootId: "root-1",
  displayName: "Notes",
  status: "connected",
  statements: [],
} as unknown as ChatFolderAccess;

function runningRun(id: string, status: AgentRun["status"]): AgentRun {
  return {
    id,
    parent_id: null,
    tier: "background",
    status,
    spawn_call_id: `call-${id}`,
  } as unknown as AgentRun;
}

function renderChip(runs: AgentRun[] = []) {
  const client = {
    listAgentRuns: vi.fn().mockResolvedValue(runs),
  } as unknown as ApiClient;
  const onOpenFolders = vi.fn();
  const onOpenAgent = vi.fn();
  render(
    <ChatStatusChip
      client={client}
      chat={chat}
      models={models}
      defaultModelKey={null}
      folders={[folder]}
      onModelChange={vi.fn()}
      onPermissionModeChange={vi.fn()}
      onOpenFolders={onOpenFolders}
      onOpenAgent={onOpenAgent}
    />,
  );
  return { onOpenFolders, onOpenAgent };
}

beforeEach(() => {
  useChatSessionStore.getState().reset();
});
afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

it("names the chat's permission mode and opens its details", async () => {
  const { onOpenFolders } = renderChip();

  const chip = screen.getByRole("button", { name: "Chat status: Auto" });
  expect(chip).toHaveTextContent("Auto");

  await userEvent.click(chip);
  expect(await screen.findByText("Model")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: /^Model:/ })).toHaveTextContent(
    "Claude Sonnet",
  );
  expect(screen.getByRole("button", { name: "Permissions: Auto" })).toBeInTheDocument();

  await userEvent.click(screen.getByText("Notes · No access"));
  expect(onOpenFolders).toHaveBeenCalled();
});

/**
 * The dots are the whole reason the chip is persistent: background work is
 * invisible from a scrolled-away transcript otherwise.
 */
it("counts live background runs and opens the newest one", async () => {
  useChatSessionStore.getState().update((session) => ({
    ...session,
    messages: [
      {
        id: "m1",
        role: "tool",
        name: "spawn_sandbox_agent",
        callId: "call-run-1",
        status: "completed",
      },
      {
        id: "m2",
        role: "tool",
        name: "spawn_sandbox_agent",
        callId: "call-run-2",
        status: "completed",
      },
    ] as never,
  }));
  const { onOpenAgent } = renderChip([
    runningRun("run-1", "running"),
    runningRun("run-2", "retry_wait"),
  ]);

  const chip = await screen.findByRole("button", {
    name: "Chat status: Auto, 2 running",
  });
  await userEvent.click(chip);
  await userEvent.click(screen.getByText("2 background agents"));
  expect(onOpenAgent).toHaveBeenCalledWith("run-2");
});
