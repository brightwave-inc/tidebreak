// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { CodeSessionSnapshot } from "../../api/types";
import {
  codeSession,
  codeWorkspace,
  deliveryCodeRepo,
} from "../../stories/fixtures";
import { useCodeCatalogStore } from "../CodeCatalogStore";
import { useWorkspaceSessions } from "./useWorkspaceSessions";

vi.mock("sonner", () => ({
  toast: { error: vi.fn(), success: vi.fn(), message: vi.fn() },
}));

const main: CodeSessionSnapshot = { ...codeSession, id: "sess-main" };
const sibling: CodeSessionSnapshot = {
  ...codeSession,
  id: "sess-sibling",
  harness_kind: "codex",
  created_at: "2026-08-20T09:10:00.000Z",
};

function setup(
  listed: CodeSessionSnapshot[],
  taskParam?: string,
  options: { failFirstLoad?: boolean } = {},
) {
  let loads = 0;
  const client = {
    getCodeWorkspace: vi.fn(async () => {
      loads += 1;
      if (options.failFirstLoad && loads === 1) throw new Error("offline");
      return codeWorkspace;
    }),
    listCodeWorkspaceSessions: vi.fn(async () => listed),
    getCodeRepo: vi.fn(async () => deliveryCodeRepo),
  };
  const navigate = vi.fn();
  const focusConversationPane = vi.fn();
  const hook = renderHook(
    ({ task }: { task: string | undefined }) =>
      useWorkspaceSessions({
        workspaceId: codeWorkspace.id,
        client: client as never,
        models: [],
        defaultModelKey: null,
        taskParam: task,
        navigate: navigate as never,
        focusConversationPane,
      }),
    { initialProps: { task: taskParam } },
  );
  return { ...hook, client, navigate, focusConversationPane };
}

function searchOf(call: unknown): Record<string, unknown> {
  const args = call as {
    search: (current: Record<string, unknown>) => Record<string, unknown>;
    replace?: boolean;
  };
  return { ...args.search({ tabs: "kept" }), replace: args.replace };
}

beforeEach(() => useCodeCatalogStore.getState().reset());
afterEach(cleanup);

describe("useWorkspaceSessions", () => {
  it("shows the first live agent when nothing is named", async () => {
    const { result } = setup([main, sibling]);
    await waitFor(() => expect(result.current.session?.id).toBe("sess-main"));
    expect(result.current.activeConversationId).toBe("sess-main");
    expect(result.current.conversationTabs.map((tab) => tab.label)).toEqual([
      "Main agent",
      "Codex CLI",
    ]);
    expect(useCodeCatalogStore.getState().sessionsByWorkspace["ws-1"]?.id).toBe(
      "sess-main",
    );
  });

  it("lets ?task= name the agent, and drops a param nothing answers to", async () => {
    const named = setup([main, sibling], "sess-sibling");
    await waitFor(() =>
      expect(named.result.current.session?.id).toBe("sess-sibling"),
    );
    expect(named.navigate).not.toHaveBeenCalled();
    named.unmount();

    const stale = setup([main], "sess-gone");
    await waitFor(() => expect(stale.navigate).toHaveBeenCalledTimes(1));
    expect(searchOf(stale.navigate.mock.calls[0]?.[0])).toEqual({
      tabs: "kept",
      task: undefined,
      subagent: undefined,
      replace: true,
    });
    expect(stale.result.current.session?.id).toBe("sess-main");
  });

  it("selects by hand through the URL and keeps the first agent unnamed", async () => {
    const { result, navigate, focusConversationPane } = setup([main, sibling]);
    await waitFor(() => expect(result.current.session?.id).toBe("sess-main"));

    act(() => result.current.selectConversation("sess-sibling"));
    expect(focusConversationPane).toHaveBeenCalledTimes(1);
    expect(result.current.session?.id).toBe("sess-sibling");
    expect(searchOf(navigate.mock.calls.at(-1)?.[0]).task).toBe("sess-sibling");

    act(() => result.current.selectConversation("sess-main"));
    expect(searchOf(navigate.mock.calls.at(-1)?.[0]).task).toBeUndefined();

    act(() => result.current.newConversation());
    expect(result.current.draftAgent).toBe(true);
    expect(result.current.session).toBeNull();
    expect(result.current.conversationTabs.at(-1)).toMatchObject({
      id: null,
      label: "New agent",
      closable: true,
    });
  });

  it("closes a sibling tab and falls back to the first agent; the first never closes", async () => {
    const { result } = setup([main, sibling]);
    await waitFor(() => expect(result.current.session?.id).toBe("sess-main"));
    act(() => result.current.selectConversation("sess-sibling"));

    act(() => result.current.closeConversation("sess-main"));
    expect(result.current.session?.id).toBe("sess-sibling");
    expect(result.current.conversationTabs).toHaveLength(2);

    act(() => result.current.closeConversation("sess-sibling"));
    expect(result.current.session?.id).toBe("sess-main");
    expect(result.current.conversationTabs.map((tab) => tab.id)).toEqual([
      "sess-main",
    ]);
  });

  it("reports a load failure and retries on request", async () => {
    const { result, client } = setup([main], undefined, {
      failFirstLoad: true,
    });
    await waitFor(() => expect(result.current.error).toBe("offline"));
    act(() => result.current.retry());
    await waitFor(() => expect(result.current.error).toBeNull());
    expect(client.getCodeWorkspace).toHaveBeenCalledTimes(2);
  });
});
