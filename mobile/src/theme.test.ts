import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { schemeOf, theme } from "./theme";

const here = dirname(fileURLToPath(import.meta.url));
const css = readFileSync(join(here, "global.css"), "utf8");

describe("theme", () => {
  it("treats anything but dark as light", () => {
    expect(schemeOf("dark")).toBe("dark");
    expect(schemeOf("light")).toBe("light");
    expect(schemeOf(null)).toBe("light");
    expect(schemeOf(undefined)).toBe("light");
  });

  it("keeps JS chrome colors in lockstep with CSS tokens", () => {
    expect(css).toContain(`--page-background: ${theme.light.pageBackground}`);
    expect(css).toContain(`--foreground: ${theme.light.foreground}`);
    expect(css).toContain(`--muted-foreground: ${theme.light.mutedForeground}`);
    expect(css).toMatch(/@media \(prefers-color-scheme:\s*dark\)/);
    expect(css).toContain(`--page-background: ${theme.dark.pageBackground}`);
    expect(css).toContain(`--foreground: ${theme.dark.foreground}`);
    expect(css).toContain(`--muted-foreground: ${theme.dark.mutedForeground}`);
  });

  it("does not reuse the light page background in dark", () => {
    expect(theme.dark.pageBackground).not.toBe(theme.light.pageBackground);
  });
});
