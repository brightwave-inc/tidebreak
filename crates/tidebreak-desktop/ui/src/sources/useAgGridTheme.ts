import { themeQuartz } from "ag-grid-community";
import { useMemo } from "react";

import { useTheme } from "@/theme";

/**
 * AG Grid dressed in the app's own tokens rather than its stock palette.
 *
 * The colours are handed over as `var(--…)` so a theme change repaints the grid
 * without rebuilding it. `browserColorScheme` is the one thing that cannot be a
 * variable — the grid uses it to pick its own scrollbars and form controls —
 * so the resolved mode is what this depends on.
 */
export function useAgGridTheme() {
  const { resolved } = useTheme();

  return useMemo(
    () =>
      themeQuartz.withParams({
        backgroundColor: "var(--background)",
        foregroundColor: "var(--foreground)",
        borderColor: "var(--border)",
        headerBackgroundColor: "var(--page-background)",
        headerTextColor: "var(--muted-foreground)",
        rowHoverColor: "var(--muted)",
        oddRowBackgroundColor: "var(--background)",
        browserColorScheme: resolved,
        fontFamily: "var(--sans)",
        headerFontSize: 14,
        fontSize: 14,
        cellHorizontalPadding: 16,
        rowHeight: 48,
        headerHeight: 40,
      }),
    [resolved],
  );
}
