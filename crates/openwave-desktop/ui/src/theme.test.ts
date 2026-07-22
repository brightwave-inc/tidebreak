import { describe, expect, it } from "vitest";
import { isThemeMode, resolveTheme } from "./theme";

describe("theme", () => {
  it("accepts only the three known modes", () => {
    expect(isThemeMode("light")).toBe(true);
    expect(isThemeMode("dark")).toBe(true);
    expect(isThemeMode("system")).toBe(true);
    expect(isThemeMode("")).toBe(false);
    expect(isThemeMode("Dark")).toBe(false);
    expect(isThemeMode(null)).toBe(false);
    expect(isThemeMode(undefined)).toBe(false);
  });

  it("resolves explicit modes without consulting the system", () => {
    expect(resolveTheme("light")).toBe("light");
    expect(resolveTheme("dark")).toBe("dark");
  });
});
