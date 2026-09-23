import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { cssVarsFor, schemeOf, theme, type ThemeTokens } from "./theme";

const here = dirname(fileURLToPath(import.meta.url));
const css = readFileSync(join(here, "global.css"), "utf8");

function assertCssHolds(tokens: ThemeTokens): void {
  const vars = cssVarsFor(tokens);
  for (const [name, value] of Object.entries(vars)) {
    expect(css).toContain(`${name}: ${value}`);
  }
}

describe("theme", () => {
  it("treats anything but dark as light", () => {
    expect(schemeOf("dark")).toBe("dark");
    expect(schemeOf("light")).toBe("light");
    expect(schemeOf(null)).toBe("light");
    expect(schemeOf(undefined)).toBe("light");
  });

  it("keeps JS tokens in lockstep with CSS", () => {
    expect(css).toMatch(/@media \(prefers-color-scheme:\s*dark\)/);
    assertCssHolds(theme.light);
    assertCssHolds(theme.dark);
  });

  it("does not reuse the light page background in dark", () => {
    expect(theme.dark.pageBackground).not.toBe(theme.light.pageBackground);
  });
});
