import type { CodeSessionSnapshot } from "../../api/types";
import { HARNESS_LABELS } from "../labels";
import type { LayoutState } from "@/panel/panelTypes";
import {
  SHELL_SHORTCUTS,
  type ShellShortcutAction,
  shortcutKeycaps,
  usesCommandModifier,
} from "@/ShellShortcuts";
import { codeBrowserIds } from "../codeChrome";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

export function useCodeShortcutHints(): { terminal: string; review: string } {
  return useMemo(() => {
    const command = usesCommandModifier(navigator.userAgent);
    return {
      terminal: shortcutHint("toggle-code-terminal", command),
      review: shortcutHint("toggle-code-review", command),
    };
  }, []);
}

export function browserTitlesForLayout(
  layout: LayoutState,
): Record<string, string> {
  return Object.fromEntries(
    codeBrowserIds(layout).map((browserId) => [browserId, "Browser"]),
  );
}

/**
 * Shells one workspace may hold at once, matching the server's own cap. The
 * plus menu stops offering another rather than letting the create fail.
 */
export const MAX_WORKSPACE_TERMINALS = 8;

/**
 * Track one element's width, in CSS pixels.
 *
 * The width stays `null` until an observer reports, so a caller can tell
 * "not measured yet" from "measured and narrow" and avoid deciding on a zero
 * it read before layout ran. The callback ref re-attaches whenever the
 * element behind it changes, which is what keeps the reading live across the
 * split going up and coming down.
 */
export function useMeasuredWidth(): {
  paneRef: (element: HTMLElement | null) => void;
  width: number | null;
} {
  const [width, setWidth] = useState<number | null>(null);
  const observerRef = useRef<ResizeObserver | null>(null);

  useEffect(() => () => observerRef.current?.disconnect(), []);

  const paneRef = useCallback((element: HTMLElement | null) => {
    observerRef.current?.disconnect();
    observerRef.current = null;
    if (!element || typeof ResizeObserver === "undefined") return;
    setWidth(element.getBoundingClientRect().width);
    const observer = new ResizeObserver((entries) => {
      const entry = entries[0];
      if (entry) setWidth(entry.contentRect.width);
    });
    observer.observe(element);
    observerRef.current = observer;
  }, []);

  return { paneRef, width };
}

/**
 * Give every open shell a tab label, keeping the ones already assigned.
 *
 * Several shells in a strip all reading "Terminal" would be untellable apart,
 * so each takes the lowest number no other open shell is using. A number is
 * released when its tab closes, which is also when its shell ends.
 */
export function nameTerminals(
  current: Readonly<Record<string, string>>,
  terminalIds: readonly string[],
): Record<string, string> {
  const kept: Record<string, string> = {};
  for (const id of terminalIds) {
    const existing = current[id];
    if (existing) kept[id] = existing;
  }
  const taken = new Set(Object.values(kept));
  for (const id of terminalIds) {
    if (kept[id]) continue;
    let ordinal = 1;
    while (taken.has(`Terminal ${ordinal}`)) ordinal += 1;
    kept[id] = `Terminal ${ordinal}`;
    taken.add(kept[id]);
  }
  const unchanged =
    Object.keys(kept).length === Object.keys(current).length &&
    Object.entries(kept).every(([id, label]) => current[id] === label);
  return unchanged ? (current as Record<string, string>) : kept;
}

/** Native child webviews must yield whenever a portaled app surface overlaps them. */

function shortcutHint(id: ShellShortcutAction, command: boolean): string {
  const def = SHELL_SHORTCUTS.find((item) => item.id === id);
  return def ? shortcutKeycaps(def, command).join("") : "";
}

/**
 * The side region beside the conversation.
 *
 * Every panel it used to draw has become a center tab, so a link naming one
 * lands here and says so rather than rendering an empty frame.
 */
export function renderCodePanel() {
  return (
    <p className="text-muted-foreground px-3 py-6 text-sm">
      This panel is not available here.
    </p>
  );
}

/**
 * A tab label for one of a workspace's agents.
 *
 * The first agent is the one the workspace was started with, so it keeps the
 * name the rest of the surface uses. The others are named by engine, numbered
 * only when the same engine runs more than once.
 */
export function conversationTabLabel(
  session: CodeSessionSnapshot,
  index: number,
  sessions: readonly CodeSessionSnapshot[],
): string {
  if (index === 0) return "Main agent";
  const label = HARNESS_LABELS[session.harness_kind];
  const same = sessions.filter(
    (entry, at) => at > 0 && entry.harness_kind === session.harness_kind,
  );
  if (same.length < 2) return label;
  return `${label} ${same.indexOf(session) + 1}`;
}
