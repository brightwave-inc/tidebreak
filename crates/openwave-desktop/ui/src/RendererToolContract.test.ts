import { describe, expect, it } from "vitest";
import { isRendererToolName } from "./api";
import { RENDERER_TOOL_NAMES } from "./generated/wire";

/**
 * The runtime guard over the generated tool vocabulary.
 *
 * The union and the list are generated together from the server's
 * `RendererToolName`, so neither can drift from the Rust enum or from each
 * other. What generation cannot check is that the guard still *rejects* —
 * `includes` over a generated list is only an allowlist as long as the typeof
 * check in front of it stays. That is what this file holds.
 *
 * Table coverage lives in `ToolIcon.test.tsx`, which walks the same list.
 */
describe("renderer tool vocabulary", () => {
  it("has a vocabulary to check against", () => {
    // Guards against the generator emitting an empty list, which would make
    // every loop below pass without checking anything.
    expect(RENDERER_TOOL_NAMES.length).toBeGreaterThan(1);
    expect(RENDERER_TOOL_NAMES).toContain("other");
  });

  it("accepts every name the server can send", () => {
    for (const name of RENDERER_TOOL_NAMES) {
      expect(isRendererToolName(name), `${name} is missing from the union`).toBe(
        true,
      );
    }
  });

  it("rejects anything the server cannot send", () => {
    // The guard is an allowlist, so a name the server folds to `other` must not
    // sneak through and become a display path for a provider-supplied string.
    for (const name of [
      "mcp__vendor__exfiltrate",
      "historical_unknown_tool",
      "",
      "SEARCH",
    ]) {
      expect(isRendererToolName(name), `${name} should not be accepted`).toBe(
        false,
      );
    }
  });

  it("rejects non-strings rather than coercing them", () => {
    // `includes` on a non-string is false, but only because the typeof check
    // runs first. Asserting it keeps that check from being dropped as redundant.
    for (const value of [null, undefined, 0, {}, ["search"]]) {
      expect(isRendererToolName(value)).toBe(false);
    }
  });

});
