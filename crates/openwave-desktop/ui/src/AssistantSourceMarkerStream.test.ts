import { describe, expect, it } from "vitest";
import { AssistantSourceMarkerStreamScrubber } from "./AssistantSourceMarkerStream";

const MARKER = "[[ow-source:0123456789abcdef0123456789abcdef]]";
const DIRECTIVE = ":cit[the reef is large]{ref=0123456789abcdef0123456789abcdef}";

describe("AssistantSourceMarkerStreamScrubber", () => {
  it("keeps a directive's phrase and drops its reference at every delta boundary", () => {
    const text = `Before ${DIRECTIVE} after.`;

    for (let boundary = 0; boundary <= text.length; boundary += 1) {
      const scrubber = new AssistantSourceMarkerStreamScrubber();
      const first = scrubber.push(text.slice(0, boundary));
      const second = scrubber.push(text.slice(boundary));
      const finished = scrubber.finish();

      expect(first).not.toContain("{ref=");
      expect(first + second + finished, `boundary ${boundary}`).toBe(
        "Before the reef is large after.",
      );
    }
  });

  it("does not flash a reference from a directive delivered one character at a time", () => {
    const scrubber = new AssistantSourceMarkerStreamScrubber();
    let visible = "";

    for (const character of `Answer ${DIRECTIVE} continues`) {
      visible += scrubber.push(character);
      expect(visible).not.toContain(":cit[");
      expect(visible).not.toContain("0123456789abcdef");
    }

    expect(visible + scrubber.finish()).toBe(
      "Answer the reef is large continues",
    );
  });

  it.each([
    ":cit[phrase]{ref=0123456789abcdef0123456789abcdeG}",
    ":cit[phrase]{ref=0123456789abcdef0123456789abcdef",
    ":cit[phrase]{cite=0123456789abcdef0123456789abcdef}",
    ":cit[phrase with ] bracket]{ref=0123456789abcdef0123456789abcdef}",
    "ordinary :citation[prose]",
  ])("preserves malformed directive-like prose: %s", (prose) => {
    const scrubber = new AssistantSourceMarkerStreamScrubber();
    const firstHalf = Math.floor(prose.length / 2);
    const visible =
      scrubber.push(prose.slice(0, firstHalf)) +
      scrubber.push(prose.slice(firstHalf)) +
      scrubber.finish();

    expect(visible).toBe(prose);
  });

  it("releases an unclosed citation phrase rather than stalling the stream", () => {
    const scrubber = new AssistantSourceMarkerStreamScrubber();
    const prose = `:cit[${"long prose ".repeat(80)}`;

    expect(scrubber.push(prose) + scrubber.finish()).toBe(prose);
  });

  it("removes a marker split at every possible delta boundary", () => {
    const text = `Before ${MARKER} after.`;

    for (let boundary = 0; boundary <= text.length; boundary += 1) {
      const scrubber = new AssistantSourceMarkerStreamScrubber();
      const first = scrubber.push(text.slice(0, boundary));
      const second = scrubber.push(text.slice(boundary));
      const finished = scrubber.finish();

      expect(first).not.toContain("[[ow-source:");
      expect(first + second + finished, `boundary ${boundary}`).toBe(
        "Before  after.",
      );
    }
  });

  it("does not flash a valid marker delivered one character at a time", () => {
    const scrubber = new AssistantSourceMarkerStreamScrubber();
    let visible = "";

    for (const character of `Answer ${MARKER} continues`) {
      visible += scrubber.push(character);
      expect(visible).not.toContain("[[");
      expect(visible).not.toContain("[[ow-source:");
      expect(visible).not.toContain("0123456789abcdef");
    }

    expect(visible + scrubber.finish()).toBe("Answer  continues");
  });

  it("removes adjacent valid markers and preserves surrounding prose", () => {
    const scrubber = new AssistantSourceMarkerStreamScrubber();
    const visible = scrubber.push(`A${MARKER}${MARKER}B`);

    expect(visible + scrubber.finish()).toBe("AB");
  });

  it("preserves a malformed marker before removing a later valid marker", () => {
    const malformed =
      "[[ow-source:0123456789abcdef0123456789abcdeG]]";
    const input = `Keep ${malformed}; hide ${MARKER}; done.`;
    const split = input.indexOf(MARKER) + 18;
    const scrubber = new AssistantSourceMarkerStreamScrubber();
    const visible =
      scrubber.push(input.slice(0, split)) +
      scrubber.push(input.slice(split)) +
      scrubber.finish();

    expect(visible).toBe(`Keep ${malformed}; hide ; done.`);
  });

  it.each([
    "[[ow-source:0123456789abcdef0123456789abcdeG]]",
    "[[ow-source:0123456789abcdef0123456789abcde]]",
    "[[ow-source:0123456789abcdef0123456789abcdef0]]",
    "[[ow-source:0123456789abcdef0123456789abcdef]x",
    "ordinary [[ow-sourcing:0123]] prose",
  ])("preserves malformed marker-like prose: %s", (prose) => {
    const scrubber = new AssistantSourceMarkerStreamScrubber();
    const firstHalf = Math.floor(prose.length / 2);
    const visible =
      scrubber.push(prose.slice(0, firstHalf)) +
      scrubber.push(prose.slice(firstHalf)) +
      scrubber.finish();

    expect(visible).toBe(prose);
  });

  it("flushes an incomplete candidate literally when a stream ends", () => {
    const scrubber = new AssistantSourceMarkerStreamScrubber();
    const incomplete = "Text [[ow-source:01234567";

    expect(scrubber.push(incomplete)).toBe("Text ");
    expect(scrubber.finish()).toBe("[[ow-source:01234567");
    expect(scrubber.finish()).toBe("");
  });

  it("flushes malformed interruption prose but never releases a valid marker", () => {
    const interrupted = new AssistantSourceMarkerStreamScrubber();
    expect(interrupted.push("Answer [[ow-source:01234567")).toBe("Answer ");
    expect(interrupted.finish()).toBe("[[ow-source:01234567");

    const completedMarker = new AssistantSourceMarkerStreamScrubber();
    expect(completedMarker.push(`Answer ${MARKER}`)).toBe("Answer ");
    expect(completedMarker.finish()).toBe("");
  });
});
