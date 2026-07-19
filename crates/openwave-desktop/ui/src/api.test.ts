import { afterEach, describe, expect, it, vi } from "vitest";
import {
  ApiClient,
  parseFolderAccessRequest,
  parsePendingToolApproval,
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
