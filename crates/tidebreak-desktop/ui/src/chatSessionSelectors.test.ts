import { describe, expect, it } from "vitest";
import { isUserMessage, stableSubset } from "./chatSessionSelectors";
import type { ChatMessage } from "./MessageList";

const question: ChatMessage = { id: "u1", role: "user", text: "Why?" };
const answer: ChatMessage = {
  id: "a1",
  role: "assistant",
  text: "Because",
  sources: [],
};

describe("stableSubset", () => {
  it("keeps its answer while a streamed token touches only other rows", () => {
    const select = stableSubset(isUserMessage);
    const first = select([question, answer]);
    const next = select([question, { ...answer, text: "Because of" }]);
    expect(next).toBe(first);
    expect(next).toEqual([question]);
  });

  it("answers anew when a kept row changes, arrives, or leaves", () => {
    const select = stableSubset(isUserMessage);
    const first = select([question, answer]);
    const edited = select([{ ...question, text: "How?" }, answer]);
    expect(edited).not.toBe(first);
    const followUp: ChatMessage = { id: "u2", role: "user", text: "And?" };
    const grown = select([...edited, answer, followUp]);
    expect(grown).toEqual([edited[0], followUp]);
    expect(select([answer])).toEqual([]);
  });
});
