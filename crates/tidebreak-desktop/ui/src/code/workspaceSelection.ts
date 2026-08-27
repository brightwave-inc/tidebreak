export type PointerSelectIntent = "open" | "toggle" | "range";

/** Cmd/Ctrl toggles. Shift ranges. A plain click still opens. */
export function pointerSelectIntent(event: {
  metaKey: boolean;
  ctrlKey: boolean;
  shiftKey: boolean;
}): PointerSelectIntent {
  if (event.shiftKey) return "range";
  if (event.metaKey || event.ctrlKey) return "toggle";
  return "open";
}

export function toggleWorkspaceSelection(
  selected: readonly string[],
  id: string,
): string[] {
  return selected.includes(id)
    ? selected.filter((item) => item !== id)
    : [...selected, id];
}

/**
 * The open workspace joins the first modifier-click so a cmd/shift gesture
 * never leaves behind the card you were already on.
 */
export function seedOpenWorkspaceSelection(
  selected: readonly string[],
  openId: string | undefined,
  visibleIds: readonly string[],
  clickedId: string,
): { selected: string[]; anchorId: string | null } {
  if (selected.length > 0) {
    return { selected: [...selected], anchorId: null };
  }
  if (!openId || openId === clickedId || !visibleIds.includes(openId)) {
    return { selected: [], anchorId: openId ?? null };
  }
  return { selected: [openId], anchorId: openId };
}

/**
 * Inclusive range from the anchor through the target in visible rail order.
 * A missing anchor or id falls back to the target alone.
 */
export function rangeWorkspaceSelection(
  visibleIds: readonly string[],
  anchorId: string | null,
  targetId: string,
): string[] {
  if (anchorId === null) return [targetId];
  const start = visibleIds.indexOf(anchorId);
  const end = visibleIds.indexOf(targetId);
  if (start < 0 || end < 0) return [targetId];
  const from = Math.min(start, end);
  const to = Math.max(start, end);
  return visibleIds.slice(from, to + 1);
}
