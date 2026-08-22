import { describe, expect, it } from "vitest";

import { forkTranscriptFile, messageWithWorkspaceFiles } from "./fork";

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

describe("forkTranscriptFile", () => {
  it("says how much of the parent the child is getting", () => {
    expect(
      forkTranscriptFile({
        path: "/private/forks/s1.md",
        byte_len: 2_048,
        turns: 12,
        truncated: false,
      }),
    ).toEqual({
      path: "/private/forks/s1.md",
      detail: "Transcript, 12 turns",
    });
  });

  /** A truncated file must not read as the whole conversation. */
  it("marks a transcript whose oldest turns were dropped", () => {
    expect(
      forkTranscriptFile({
        path: "/private/forks/s2.md",
        byte_len: 524_288,
        turns: 1,
        truncated: true,
      }).detail,
    ).toBe("Transcript, most recent 1 turn");
  });
});
