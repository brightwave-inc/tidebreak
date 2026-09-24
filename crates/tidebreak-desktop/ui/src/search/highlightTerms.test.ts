// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";

import { termRanges, termSpans } from "./highlightTerms";
import { useTranscriptFindStore } from "./transcriptFind";

function marked(text: string, terms: string[]): string[] {
  return termSpans(text, terms).map((span) => text.slice(span.start, span.end));
}

describe("marking a query's words on screen", () => {
  it("marks words that start with a term, folded the way the index folds", () => {
    expect(marked("The Café opens; cafeteria next door", ["cafe"])).toEqual([
      "Café",
      "cafe",
    ]);
  });

  it("marks a camelCase part and a name's segments, not a word inside a word", () => {
    expect(marked("SubmitButton and submit_button", ["button"])).toEqual([
      "Button",
      "button",
    ]);
    expect(marked("nextest runs tests", ["test"])).toEqual(["test"]);
  });

  it("marks ranges inside the DOM without changing it", () => {
    const root = document.createElement("div");
    root.innerHTML = "<p>Harbour <b>fees</b> and harbour dues</p>";
    const before = root.innerHTML;
    const ranges = termRanges(root, ["harbour", "fees"]);
    expect(ranges.map((range) => range.toString())).toEqual([
      "Harbour",
      "fees",
      "harbour",
    ]);
    expect(root.innerHTML).toBe(before);
  });
});

describe("which transcript Cmd+F opens in", () => {
  afterEach(() =>
    useTranscriptFindStore.setState({ hosts: [], openRequest: null }),
  );

  it("declines with no transcript on screen", () => {
    expect(useTranscriptFindStore.getState().requestOpen()).toBe(false);
  });

  it("opens in the transcript registered or focused last", () => {
    const store = useTranscriptFindStore.getState();
    const releaseChat = store.register("chat:a");
    store.register("code:b");
    expect(store.requestOpen()).toBe(true);
    expect(useTranscriptFindStore.getState().openRequest?.hostId).toBe(
      "code:b",
    );
    store.activate("chat:a");
    store.requestOpen();
    expect(useTranscriptFindStore.getState().openRequest?.hostId).toBe(
      "chat:a",
    );
    releaseChat();
    expect(useTranscriptFindStore.getState().openRequest).toBeNull();
    store.requestOpen();
    expect(useTranscriptFindStore.getState().openRequest?.hostId).toBe(
      "code:b",
    );
  });
});
