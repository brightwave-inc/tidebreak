import { describe, expect, it } from "vitest";
import { AssistantSourceMarkerStreamScrubber } from "./AssistantSourceMarkerStream";

const MARKER = "[[ow-source:0123456789abcdef0123456789abcdef]]";

describe("AssistantSourceMarkerStreamScrubber", () => {
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
});
