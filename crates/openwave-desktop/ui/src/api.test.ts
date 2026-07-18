import { describe, expect, it } from "vitest";
import { parseFolderAccessRequest } from "./api";

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
