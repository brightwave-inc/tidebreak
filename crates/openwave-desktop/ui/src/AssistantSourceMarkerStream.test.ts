import { describe, expect, it } from "vitest";
import { AssistantSourceMarkerStreamScrubber } from "./AssistantSourceMarkerStream";

const DOCUMENT = "0b2b1f2c-9d3e-4a5b-8c7d-6e5f4a3b2c1d";
const DIRECTIVE = `:cit[the reef is large]{doc=${DOCUMENT} pages=3-4}`;

describe("AssistantSourceMarkerStreamScrubber", () => {
  it("keeps the phrase and masks locator markup at every delta boundary", () => {
    const text = `Before ${DIRECTIVE} after.`;
    for (let boundary = 0; boundary <= text.length; boundary += 1) {
      const scrubber = new AssistantSourceMarkerStreamScrubber();
      const visible =
        scrubber.push(text.slice(0, boundary)) +
        scrubber.push(text.slice(boundary)) +
        scrubber.finish();
      expect(visible, `boundary ${boundary}`).toBe(
        "Before the reef is large after.",
      );
    }
  });

  it("does not flash a directive delivered one character at a time", () => {
    const scrubber = new AssistantSourceMarkerStreamScrubber();
    let visible = "";
    for (const character of `Answer ${DIRECTIVE} continues`) {
      visible += scrubber.push(character);
      expect(visible).not.toContain(":cit[");
      expect(visible).not.toContain(DOCUMENT);
    }
    expect(visible + scrubber.finish()).toBe(
      "Answer the reef is large continues",
    );
  });

  it("flushes an interrupted directive literally", () => {
    const scrubber = new AssistantSourceMarkerStreamScrubber();
    expect(scrubber.push("Answer :cit[the reef")).toBe("Answer ");
    expect(scrubber.finish()).toBe(":cit[the reef");
    expect(scrubber.finish()).toBe("");
  });

  it("preserves directive-shaped prose that cannot become a directive", () => {
    const scrubber = new AssistantSourceMarkerStreamScrubber();
    const prose = "ordinary :cit[prose] without attributes";
    expect(scrubber.push(prose) + scrubber.finish()).toBe(prose);
  });
});
