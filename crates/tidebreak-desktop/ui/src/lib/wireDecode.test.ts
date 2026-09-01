import { describe, expect, it } from "vitest";

import {
  MAX_WIRE_CURSOR_CHARS,
  MAX_WIRE_ID_CHARS,
  MAX_WIRE_TIMESTAMP_CHARS,
  bounded,
  boundedBlock,
  isFiniteNumber,
  isMember,
  isNonNegativeInteger,
  isPositiveInteger,
  isStringList,
  isWebUrl,
  nonEmptyBounded,
  nonEmptyString,
  nullableNonEmptyString,
  nullableString,
  optionalString,
} from "./wireDecode";

describe("bounded readers", () => {
  it("count code points, not UTF-16 units", () => {
    // Four astral characters are eight UTF-16 units but four code points.
    const emoji = "😀😀😀😀";
    expect(emoji.length).toBe(8);
    expect(bounded(emoji, 4)).toBe(true);
    expect(bounded(emoji, 3)).toBe(false);
  });

  it("reject the characters that could redraw or reorder a line", () => {
    for (const forbidden of [
      "a\u0000b", // NUL
      "a\u001bb", // ESC
      "a\u007fb", // DEL
      "a\u0085b", // C1 NEL
      "a\u2028b", // line separator
      "a\u2029b", // paragraph separator
      "a\u202eb", // right-to-left override
      "a\u2066b", // left-to-right isolate
      "a\nb",
      "a\tb",
    ]) {
      expect(bounded(forbidden, 100)).toBe(false);
    }
    expect(bounded("plain ascii, punctuation; and “quotes”", 100)).toBe(true);
  });

  it("boundedBlock keeps newlines and tabs as structure but nothing else", () => {
    expect(boundedBlock("line one\n\tline two", 100)).toBe(true);
    expect(boundedBlock("a\u001b[31mred", 100)).toBe(false);
    expect(boundedBlock("a\u202eb", 100)).toBe(false);
    expect(boundedBlock("x".repeat(5), 4)).toBe(false);
  });

  it("nonEmptyBounded rejects blank strings that bounded accepts", () => {
    expect(bounded("", 10)).toBe(true);
    expect(bounded("   ", 10)).toBe(true);
    expect(nonEmptyBounded("", 10)).toBe(false);
    expect(nonEmptyBounded("   ", 10)).toBe(false);
    expect(nonEmptyBounded(" x ", 10)).toBe(true);
  });

  it("only accept strings", () => {
    for (const notString of [null, undefined, 1, true, [], {}]) {
      expect(bounded(notString, 10)).toBe(false);
      expect(boundedBlock(notString, 10)).toBe(false);
      expect(nonEmptyBounded(notString, 10)).toBe(false);
    }
  });
});

describe("shared guard limits", () => {
  it("fit an RFC 3339 timestamp and a UUID with room to spare", () => {
    expect(
      nonEmptyBounded(
        "2026-09-01T17:02:14.123456789+00:00",
        MAX_WIRE_TIMESTAMP_CHARS,
      ),
    ).toBe(true);
    expect(
      nonEmptyBounded(
        "0b0d9f2e-6f6c-4c3f-9a4e-7c1b2d3e4f5a",
        MAX_WIRE_ID_CHARS,
      ),
    ).toBe(true);
  });

  it("reject a payload that pads a field past the limit", () => {
    expect(
      nonEmptyBounded("x".repeat(MAX_WIRE_ID_CHARS), MAX_WIRE_ID_CHARS),
    ).toBe(true);
    expect(
      nonEmptyBounded("x".repeat(MAX_WIRE_ID_CHARS + 1), MAX_WIRE_ID_CHARS),
    ).toBe(false);
    expect(
      nonEmptyBounded(
        "x".repeat(MAX_WIRE_TIMESTAMP_CHARS + 1),
        MAX_WIRE_TIMESTAMP_CHARS,
      ),
    ).toBe(false);
    expect(
      nonEmptyBounded(
        "x".repeat(MAX_WIRE_CURSOR_CHARS + 1),
        MAX_WIRE_CURSOR_CHARS,
      ),
    ).toBe(false);
  });
});

describe("presence-only readers", () => {
  it("keep the two null-handling conventions apart", () => {
    // Code-mode decoder: any string, null allowed, key must be present.
    expect(nullableString("")).toBe(true);
    expect(nullableString(null)).toBe(true);
    expect(nullableString(undefined)).toBe(false);
    // Chat decoder: null or a non-empty string.
    expect(nullableNonEmptyString("")).toBe(false);
    expect(nullableNonEmptyString(null)).toBe(true);
    expect(nullableNonEmptyString(undefined)).toBe(false);
    expect(nullableNonEmptyString("set")).toBe(true);
  });

  it("optionalString accepts absence but not null", () => {
    expect(optionalString(undefined)).toBe(true);
    expect(optionalString("")).toBe(true);
    expect(optionalString(null)).toBe(false);
  });

  it("nonEmptyString counts whitespace as content", () => {
    expect(nonEmptyString(" ")).toBe(true);
    expect(nonEmptyString("")).toBe(false);
    expect(nonEmptyString(0)).toBe(false);
  });

  it("isStringList rejects a single non-string entry", () => {
    expect(isStringList([])).toBe(true);
    expect(isStringList(["a", "b"])).toBe(true);
    expect(isStringList(["a", 1])).toBe(false);
    expect(isStringList("a")).toBe(false);
  });
});

describe("enum and number readers", () => {
  it("isMember rejects non-strings even when the set would coerce them", () => {
    const set = new Set(["1", "open"]);
    expect(isMember("open", set)).toBe(true);
    expect(isMember(1, set)).toBe(false);
    expect(isMember("closed", set)).toBe(false);
  });

  it("integer readers reject floats, unsafe integers, and numeric strings", () => {
    expect(isNonNegativeInteger(0)).toBe(true);
    expect(isNonNegativeInteger(-1)).toBe(false);
    expect(isNonNegativeInteger(1.5)).toBe(false);
    expect(isNonNegativeInteger(2 ** 53)).toBe(false);
    expect(isNonNegativeInteger("3")).toBe(false);
    expect(isPositiveInteger(1)).toBe(true);
    expect(isPositiveInteger(0)).toBe(false);
    expect(isFiniteNumber(1.5)).toBe(true);
    expect(isFiniteNumber(Number.NaN)).toBe(false);
    expect(isFiniteNumber(Number.POSITIVE_INFINITY)).toBe(false);
  });

  it("isWebUrl admits only absolute http(s) addresses", () => {
    expect(isWebUrl("https://example.com/x")).toBe(true);
    expect(isWebUrl("http://example.com")).toBe(true);
    expect(isWebUrl("javascript:alert(1)")).toBe(false);
    expect(isWebUrl("file:///etc/passwd")).toBe(false);
    expect(isWebUrl("/relative")).toBe(false);
    expect(isWebUrl(42)).toBe(false);
  });
});
