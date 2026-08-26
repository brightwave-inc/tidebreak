import { describe, expect, it } from "vitest";
import { initialTranscript, reduceTranscript } from "./transcript";

describe("reduceTranscript", () => {
  it("ignores duplicate durable seq", () => {
    let state = initialTranscript();
    state = reduceTranscript(state, {
      seq: 1,
      event: { type: "turn_started", turn_id: "t1" },
    });
    const again = reduceTranscript(state, {
      seq: 1,
      event: { type: "turn_started", turn_id: "t1" },
    });
    expect(again).toBe(state);
    expect(again.lastSeq).toBe(1);
  });

  it("does not advance lastSeq on transient assistant deltas", () => {
    let state = reduceTranscript(initialTranscript(), {
      seq: 4,
      event: { type: "turn_started", turn_id: "t1" },
    });
    state = reduceTranscript(state, {
      seq: 4,
      transient: true,
      event: { type: "assistant_delta", text: "Hello" },
    });
    expect(state.lastSeq).toBe(4);
    const assistant = state.items.find((item) => item.kind === "assistant");
    expect(assistant?.kind === "assistant" && assistant.text).toBe("Hello");
  });

  it("collapses tool start/complete into one card", () => {
    let state = reduceTranscript(initialTranscript(), {
      seq: 1,
      event: {
        type: "tool_started",
        call_id: "c1",
        name: "Bash",
        detail: { kind: "command", cmd: "ls", cwd: "." },
      },
    });
    state = reduceTranscript(state, {
      seq: 2,
      event: {
        type: "tool_completed",
        call_id: "c1",
        outcome: "succeeded",
        preview: "ok",
      },
    });
    const tools = state.items.filter((item) => item.kind === "tool");
    expect(tools).toHaveLength(1);
    expect(tools[0]).toMatchObject({
      name: "Bash",
      summary: "ok",
    });
  });
});
