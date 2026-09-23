/**
 * Colors used from JavaScript (navigation chrome, StatusBar, placeholders,
 * and icons). Keep these in lockstep with `global.css`.
 */
export const theme = {
  light: {
    pageBackground: "#f4f5f7",
    foreground: "#1b1d22",
    mutedForeground: "#6b7280",
  },
  dark: {
    pageBackground: "#1c1e22",
    foreground: "#f4f5f7",
    mutedForeground: "#9ca3af",
  },
} as const;

export type ColorSchemeName = "light" | "dark";

export function schemeOf(value: string | null | undefined): ColorSchemeName {
  return value === "dark" ? "dark" : "light";
}
