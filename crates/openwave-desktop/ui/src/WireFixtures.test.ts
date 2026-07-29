import { describe, expect, it } from "vitest";
import {
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
      preview: { tool: "exec", command: "git", args: ["status"], cwd: "." },
      canApprove: true,
      canRemember: true,
      // The fixture is real server output: `git` is a one-token prefix rung.
      prefixRungs: [1],
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
