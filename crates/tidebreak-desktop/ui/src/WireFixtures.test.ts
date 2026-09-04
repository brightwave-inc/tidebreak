import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { metadataFrame } from "./ChatSessionController";
import {
  initialChatSessionState,
  reduceChatSessionEvent,
} from "./ChatSessionReducer";
import {
  type ChatFrame,
  RENDERER_FOLDER_ACCESS_REASON,
  parseFolderAccessRequest,
  parsePendingToolApproval,
} from "./api";
import {
  PENDING_APPROVAL,
  PENDING_APPROVAL_WITHOUT_PREVIEW,
  PENDING_FOLDER_ACCESS_REQUEST,
} from "./generated/fixtures";

/**
 * One real frame of every kind the chat socket carries, serialized by the
 * server's own types. The same file is decoded by the CLI's tests and by the
 * server's round trip, so the three readers of this socket cannot drift apart
 * without one of them failing.
 */
const CHAT_FRAMES: { name: string; frame: ChatFrame }[] = JSON.parse(
  readFileSync(
    fileURLToPath(
      new URL(
        "../../../tidebreak-server-api/fixtures/chat-frames.json",
        import.meta.url,
      ),
    ),
    "utf8",
  ),
);

/**
 * The validators, run against the server's real output.
 *
 * Every other test of these functions builds its own input, which means it
 * encodes what the author believed the wire looked like. That is how a field
 * renamed server-side leaves both test suites green and the app broken — the
 * failure mode this whole effort exists to remove, and the one place generated
 * types cannot help, because these validators are a trust boundary and stay
 * hand-written.
 *
 * The fixtures here are serialized from the server's own types by a Rust test,
 * so they cannot drift from it. A rename changes the fixture, and these fail.
 *
 * Rejection cases deliberately stay hand-authored elsewhere: malformed input is
 * not something the server can produce, so there is nothing to generate.
 */
describe("validators against real server output", () => {
  it("accepts a pending approval and remaps it to the app shape", () => {
    const parsed = parsePendingToolApproval(PENDING_APPROVAL);

    expect(parsed).not.toBeNull();
    expect(parsed).toEqual({
      callId: "00000000-0000-0000-0000-000000000001",
      turnId: "00000000-0000-0000-0000-000000000002",
      action: "exec",
      approval: "exec_may_run_networked_command",
      class: "sensitive",
      preview: {
        tool: "exec",
        command: "git",
        args: ["status"],
        cwd: ".",
        files: ["documents/report.pdf"],
        summary: "Checking the repository status",
      },
      canApprove: true,
      canRemember: true,
      grantRungs: [
        "exact_action",
        { command_prefix: { tokens: 1 } },
        "whole_tool",
      ],
      autoJudgeStatus: null,
    });
  });

  it("accepts an approval whose optional preview key is absent", () => {
    // The server omits `preview` rather than sending null. Hand-authored inputs
    // habitually send null instead, so this case went untested.
    expect("preview" in PENDING_APPROVAL_WITHOUT_PREVIEW).toBe(false);

    const parsed = parsePendingToolApproval(PENDING_APPROVAL_WITHOUT_PREVIEW);

    expect(parsed).not.toBeNull();
    expect(parsed?.preview).toBeNull();
    expect(parsed?.canApprove).toBe(false);
    expect(parsed?.canRemember).toBe(false);
  });

  it("accepts a folder access request and renames claimed for the app", () => {
    const parsed = parseFolderAccessRequest(PENDING_FOLDER_ACCESS_REQUEST);

    expect(parsed).not.toBeNull();
    expect(parsed).toEqual({
      callId: "00000000-0000-0000-0000-000000000003",
      turnId: "00000000-0000-0000-0000-000000000004",
      reason: RENDERER_FOLDER_ACCESS_REASON,
      folderHint: "documents",
      claimedByDesktop: true,
    });
  });

  it("agrees with the server on the frozen consent prose", () => {
    // The validator rejects any request whose reason is not byte-identical to
    // this constant, so no server-authored text can reach a consent prompt. The
    // two literals live in different languages; the fixture is what ties them.
    expect(PENDING_FOLDER_ACCESS_REQUEST.reason).toBe(
      RENDERER_FOLDER_ACCESS_REASON,
    );
  });
});

describe("chat frames against real server output", () => {
  it("carries every frame kind", () => {
    expect(CHAT_FRAMES.length).toBeGreaterThan(20);
    const tags = new Set(
      CHAT_FRAMES.map(({ frame }) =>
        "event" in frame ? frame.event.type : `metadata:${frame.metadata}`,
      ),
    );
    expect(tags.has("turn_started")).toBe(true);
    expect(tags.has("tool_call_completed")).toBe(true);
    expect(tags.has("metadata:titled")).toBe(true);
  });

  it("reduces every journaled frame without throwing", () => {
    // The renderer parses each frame with a cast and no runtime validation,
    // so the reducer is the first code that reads the payload. Every real
    // frame has to be something it can take, in journal order.
    let state = initialChatSessionState();
    const deps = { nextId: () => "id", now: () => "2026-09-01T00:00:00Z" };
    let reduced = 0;
    for (const { name, frame } of CHAT_FRAMES) {
      if (!("event" in frame)) continue;
      const transition = reduceChatSessionEvent(state, frame, deps);
      expect(transition.state.lastSeq, name).toBe(frame.seq);
      state = transition.state;
      reduced += 1;
    }
    expect(reduced).toBeGreaterThan(20);
  });

  it("recognizes every metadata frame and no event frame as metadata", () => {
    for (const { name, frame } of CHAT_FRAMES) {
      const metadata = metadataFrame(frame);
      if ("metadata" in frame) {
        expect(metadata, name).toEqual(frame);
      } else {
        expect(metadata, name).toBeNull();
      }
    }
  });
});
