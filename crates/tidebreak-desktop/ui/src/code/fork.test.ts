import { describe, expect, it } from "vitest";

import {
  forkFraming,
  forkTranscriptFile,
  messageWithWorkspaceFiles,
} from "./fork";

describe("messageWithWorkspaceFiles", () => {
  it("leaves a message alone when nothing is attached", () => {
    expect(messageWithWorkspaceFiles("Keep going.", [])).toBe("Keep going.");
  });

  /**
   * The paths are the whole handoff. A dropped or reworded path breaks the
   * fork silently.
   */
  it("names every attached path after the message", () => {
    expect(
      messageWithWorkspaceFiles("Read this first.", [
        { path: "/private/forks/a.md" },
        { path: "docs/plan.md", detail: "ignored here" },
      ]),
    ).toBe(
      "Read this first.\n\nFiles available to you:\n" +
        "- `/private/forks/a.md`\n- `docs/plan.md`",
    );
  });
});

describe("forkFraming", () => {
  it("tells the child to read the transcript before the records", () => {
    const framing = forkFraming({
      path: "/private/forks/s1/g1/transcript.md",
      dir: "/private/forks/s1/g1",
      byte_len: 2_048,
      turns: 12,
      total_turns: 12,
      truncated: false,
    });
    expect(framing).toContain("Read the attached transcript first");
    expect(framing).toContain("full record");
    expect(framing).not.toContain("fork was taken");
  });

  /**
   * A fork point shares its worktree with the turns it excluded. The child
   * must hear that the files can be ahead of the transcript, or it will
   * treat someone else's work as its own.
   */
  it("warns about later work when the fork stopped before the newest turn", () => {
    const framing = forkFraming({
      path: "/private/forks/s1/g2/transcript.md",
      dir: "/private/forks/s1/g2",
      byte_len: 2_048,
      turns: 7,
      total_turns: 7,
      at_turn_ordinal: 7,
      truncated: false,
    });
    expect(framing).toContain("ends at turn 7");
    expect(framing).toContain("worktree may hold changes");
  });
});

describe("forkTranscriptFile", () => {
  it("says how much of the parent the child is getting", () => {
    expect(
      forkTranscriptFile({
        path: "/private/forks/s1/g1/transcript.md",
        dir: "/private/forks/s1/g1",
        byte_len: 2_048,
        turns: 12,
        total_turns: 12,
        truncated: false,
      }),
    ).toEqual({
      path: "/private/forks/s1/g1/transcript.md",
      detail: "Transcript, 12 turns",
    });
  });

  /** A truncated file must not read as the whole conversation. */
  it("marks a transcript whose oldest turns were reduced", () => {
    expect(
      forkTranscriptFile({
        path: "/private/forks/s2/g1/transcript.md",
        dir: "/private/forks/s2/g1",
        byte_len: 524_288,
        turns: 1,
        total_turns: 40,
        truncated: true,
      }).detail,
    ).toBe("Transcript, most recent 1 turn");
  });
});
