import { afterEach, describe, expect, it, vi } from "vitest";
import {
  ApiClient,
  parseFolderAccessRequest,
  parsePendingToolApproval,
  parseSandboxAgentCancellation,
} from "./api";

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("parseFolderAccessRequest", () => {
  it("accepts only the closed renderer-safe consent projection", () => {
    expect(
      parseFolderAccessRequest({
        call_id: "call-1",
        turn_id: "turn-1",
        reason:
          "The assistant needs read access to files outside the folders connected to this conversation.",
        folder_hint: "documents",
        claimed: true,
      }),
    ).toEqual({
      callId: "call-1",
      turnId: "turn-1",
      reason:
        "The assistant needs read access to files outside the folders connected to this conversation.",
      folderHint: "documents",
      claimedByDesktop: true,
    });

    expect(
      parseFolderAccessRequest({
        call_id: "call-2",
        turn_id: "turn-2",
        reason:
          "The assistant needs read access to files outside the folders connected to this conversation.",
        folder_hint: null,
        claimed: false,
      }),
    ).toEqual({
      callId: "call-2",
      turnId: "turn-2",
      reason:
        "The assistant needs read access to files outside the folders connected to this conversation.",
      folderHint: null,
      claimedByDesktop: false,
    });
  });

  it("rejects canonical tool records and extended projections", () => {
    expect(
      parseFolderAccessRequest({
        call_id: "call-1",
        turn_id: "turn-1",
        reason:
          "The assistant needs read access to files outside the folders connected to this conversation.",
        folder_hint: null,
        claimed: false,
        arguments: { path: "/Users/private" },
      }),
    ).toBeNull();

    expect(
      parseFolderAccessRequest({
        id: "call-1",
        chat_id: "chat-1",
        turn_id: "turn-1",
        name: "request_folder_access",
        arguments: { reason: "Read files" },
        provider_id: "provider-secret",
        client_executor_id: "executor-secret",
      }),
    ).toBeNull();
  });

  it("rejects malformed user-facing fields", () => {
    expect(
      parseFolderAccessRequest({
        call_id: "call-1",
        turn_id: "turn-1",
        reason: "Model-controlled secret prose",
        folder_hint: null,
        claimed: false,
      }),
    ).toBeNull();

    expect(
      parseFolderAccessRequest({
        call_id: "call-1",
        turn_id: "turn-1",
        reason: " ",
        folder_hint: "desktop",
        claimed: "yes",
      }),
    ).toBeNull();
  });
});

describe("pending approval recovery", () => {
  const safe = {
    call_id: "call-1",
    turn_id: "turn-1",
    action: "search",
    approval: "search_may_share_query_and_excerpts",
    class: "sensitive",
    can_approve: true,
  };

  it("parses only the closed renderer projection", () => {
    expect(parsePendingToolApproval(safe)).toEqual({
      callId: "call-1",
      turnId: "turn-1",
      action: "search",
      approval: "search_may_share_query_and_excerpts",
      class: "sensitive",
      canApprove: true,
    });
    expect(parsePendingToolApproval({ ...safe, arguments: { query: "private" } })).toBeNull();
    expect(parsePendingToolApproval({ ...safe, can_approve: false })).toBeNull();
    expect(parsePendingToolApproval({ ...safe, action: "private_plugin" })).toBeNull();
  });

  it("recognizes the fixed background wait name without accepting extensions", () => {
    expect(
      parsePendingToolApproval({
        ...safe,
        action: "wait_for_agents",
        approval: "unsupported",
        can_approve: false,
      }),
    ).toMatchObject({ action: "wait_for_agents", canApprove: false });
    expect(
      parsePendingToolApproval({
        ...safe,
        action: "wait_for_agents_with_private_results",
        approval: "unsupported",
        can_approve: false,
      }),
    ).toBeNull();
  });

  it("fails closed on malformed, duplicate, or cross-turn pages", async () => {
    const client = new ApiClient("http://127.0.0.1", "token");
    for (const body of [
      { approval: safe },
      [{ ...safe, arguments: { query: "private" } }],
      [safe, safe],
      [safe, { ...safe, call_id: "call-2", turn_id: "turn-2" }],
    ]) {
      vi.stubGlobal(
        "fetch",
        vi.fn().mockResolvedValue(
          new Response(JSON.stringify(body), {
            status: 200,
            headers: { "Content-Type": "application/json" },
          }),
        ),
      );
      await expect(client.listPendingApprovals("chat-1")).rejects.toThrow(
        /pending approval response/,
      );
    }
  });
});

describe("active turn steering", () => {
  it("posts an interrupt against the exact chat, turn, and stable identity", async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(null, { status: 202 }));
    vi.stubGlobal("fetch", fetchMock);
    const client = new ApiClient("http://127.0.0.1", "token");

    await client.steer(
      "chat-1",
      "turn-1",
      "steer-1",
      "change course",
      true,
    );

    expect(fetchMock).toHaveBeenCalledTimes(1);
    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("http://127.0.0.1/chats/chat-1/steer");
    expect(init.method).toBe("POST");
    expect(JSON.parse(String(init.body))).toEqual({
      steer_id: "steer-1",
      turn_id: "turn-1",
      content: "change course",
      interrupt: true,
    });
  });
});

describe("sandbox agent cancellation", () => {
  it("accepts only the closed cancellation projection", () => {
    expect(
      parseSandboxAgentCancellation({ id: "run-1", status: "cancelling" }),
    ).toEqual({ id: "run-1", status: "cancelling" });
    expect(
      parseSandboxAgentCancellation({ id: "run-1", status: "cancelled" }),
    ).toEqual({ id: "run-1", status: "cancelled" });

    for (const body of [
      { id: "run-1", status: "running" },
      { id: "", status: "cancelled" },
      { id: "run-1", status: "cancelled", lease_token: "private" },
      { id: "run-1", status: "cancelled", executor_id: "private" },
      { id: "run-1", status: "cancelled", diagnostic: "private" },
    ]) {
      expect(parseSandboxAgentCancellation(body)).toBeNull();
    }
  });

  it("posts to the exact encoded run route and parses a 202 response body", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(JSON.stringify({ id: "run/1", status: "cancelling" }), {
        status: 202,
        headers: { "Content-Type": "application/json" },
      }),
    );
    vi.stubGlobal("fetch", fetchMock);
    const client = new ApiClient("http://127.0.0.1", "token");

    await expect(client.cancelAgentRun("chat/1", "run/1")).resolves.toEqual({
      id: "run/1",
      status: "cancelling",
    });
    expect(fetchMock).toHaveBeenCalledWith(
      "http://127.0.0.1/chats/chat%2F1/agent-runs/run%2F1/cancel",
      expect.objectContaining({ method: "POST" }),
    );
  });

  it("fails closed when the server returns another run identity", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response(JSON.stringify({ id: "run-2", status: "cancelled" }), {
          status: 202,
        }),
      ),
    );
    const client = new ApiClient("http://127.0.0.1", "token");

    await expect(client.cancelAgentRun("chat-1", "run-1")).rejects.toThrow(
      "sandbox cancellation response is invalid",
    );
  });

  it("rejects a valid projection returned with a status other than 202", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response(JSON.stringify({ id: "run-1", status: "cancelled" }), {
          status: 200,
        }),
      ),
    );
    const client = new ApiClient("http://127.0.0.1", "token");

    await expect(client.cancelAgentRun("chat-1", "run-1")).rejects.toThrow(
      "expected 202, received 200",
    );
  });
});
