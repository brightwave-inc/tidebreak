import { describe, expect, it } from "vitest";

import { messageSearchHit } from "../stories/fixtures";
import {
  foldText,
  hitKey,
  hitRoute,
  hitTarget,
  hitTitle,
  queryTerms,
  snippetSegments,
} from "./messageSearch";

describe("snippet segments", () => {
  it("cuts a snippet at its matched ranges, in UTF-16 units", () => {
    // An emoji is two UTF-16 units, so a range past it lands where
    // JavaScript counts, not where a code-point count would.
    const snippet = "🌊 Harbour dues and harbour fees";
    const first = snippet.indexOf("Harbour");
    const second = snippet.indexOf("harbour");
    expect(
      snippetSegments(snippet, [
        { start: first, end: first + 7 },
        { start: second, end: second + 7 },
      ]),
    ).toEqual([
      { text: "🌊 ", match: false },
      { text: "Harbour", match: true },
      { text: " dues and ", match: false },
      { text: "harbour", match: true },
      { text: " fees", match: false },
    ]);
  });

  it("never throws or repeats text on ranges out of order, overlapping, or past the end", () => {
    const segments = snippetSegments("abcdef", [
      { start: 4, end: 99 },
      { start: 1, end: 3 },
      { start: 2, end: 5 },
      { start: 3, end: 3 },
    ]);
    expect(segments.map((segment) => segment.text).join("")).toBe("abcdef");
    expect(segments.filter((segment) => segment.match)).toEqual([
      { text: "bc", match: true },
      { text: "de", match: true },
      { text: "f", match: true },
    ]);
  });

  it("leaves a snippet with no ranges whole", () => {
    expect(snippetSegments("plain", [])).toEqual([
      { text: "plain", match: false },
    ]);
  });
});

describe("what a hit opens", () => {
  it("opens a chat hit at its message", () => {
    const hit = messageSearchHit("harbour", "harbour");
    expect(hitTarget(hit)).toEqual({
      kind: "chat",
      sessionId: hit.session_id,
      messageId: hit.message_id,
      turnId: hit.turn_id,
    });
    expect(hitRoute(hitTarget(hit)!)).toBe(`/c/${hit.session_id}`);
  });

  it("opens a code hit at its event, in its workspace when it has one", () => {
    const hit = messageSearchHit("cargo test", "cargo", {
      kind: "code",
      message_id: undefined,
      workspace_id: "ws-1",
      event_seq: 42,
    });
    const target = hitTarget(hit)!;
    expect(target).toMatchObject({ kind: "code", eventSeq: 42 });
    expect(hitRoute(target)).toBe(`/code/w/ws-1?task=${hit.session_id}`);
    expect(hitRoute(hitTarget({ ...hit, workspace_id: undefined })!)).toBe(
      `/code/s/${hit.session_id}`,
    );
  });

  it("keys hits apart by where they point", () => {
    const chat = messageSearchHit("a", "a");
    const code = messageSearchHit("b", "b", {
      message_id: undefined,
      event_seq: 7,
    });
    expect(hitKey(chat)).not.toBe(hitKey(code));
    expect(hitKey(code)).toContain("event:7");
  });

  it("names an untitled conversation the way the rail does", () => {
    expect(hitTitle({ kind: "chat", title: "  " })).toBe("New work");
    expect(hitTitle({ kind: "code" })).toBe("Untitled session");
  });
});

describe("query terms", () => {
  it("folds words the way the index does", () => {
    expect(queryTerms("Café  ﬁle-name CAFE")).toEqual(["cafe", "file", "name"]);
    expect(foldText("Ångström")).toBe("angstrom");
  });
});
