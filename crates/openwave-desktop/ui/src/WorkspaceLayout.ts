import { normalizeSurface, type Surface, type SurfaceKind } from "./Surface";

/**
 * How the workspace is arranged for one chat: which surface is open beside the
 * transcript, and whether it has been expanded over it. The transcript stays
 * mounted while a surface is open, so this is a layout, not a selection.
 */
export type ChatLayout = {
  surface: Surface;
  expanded: boolean;
};

export type WorkspaceLayouts = {
  /** Share of the workspace given to the side panel, as a fraction of width. */
  fraction: number;
  chats: Record<string, ChatLayout>;
};

/** Surfaces that open beside the transcript rather than replacing it. */
const SIDE_PANEL_SURFACES: ReadonlySet<SurfaceKind> = new Set([
  "documents",
  "deliverables",
  "folders",
]);

export const MIN_FRACTION = 0.25;
export const MAX_FRACTION = 0.75;
export const DEFAULT_FRACTION = 0.42;

/**
 * Layouts are remembered per chat, so the map has to be bounded — a long-lived
 * install would otherwise accumulate an entry for every conversation ever
 * opened, including deleted ones.
 */
export const MAX_REMEMBERED_CHATS = 50;

export const WORKSPACE_LAYOUT_KEY = "openwave.workspace-layout";

export function opensBesideTranscript(kind: SurfaceKind): boolean {
  return SIDE_PANEL_SURFACES.has(kind);
}

export function clampFraction(value: unknown): number {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return DEFAULT_FRACTION;
  }
  return Math.min(MAX_FRACTION, Math.max(MIN_FRACTION, value));
}

export const EMPTY_LAYOUTS: WorkspaceLayouts = {
  fraction: DEFAULT_FRACTION,
  chats: {},
};

/**
 * Reduce stored JSON to layouts this build can render. Anything unrecognised
 * is dropped rather than repaired: a restored layout that cannot be trusted
 * should leave the reader on the transcript.
 */
export function normalizeLayouts(value: unknown): WorkspaceLayouts {
  if (!isRecord(value)) return EMPTY_LAYOUTS;

  const chats: Record<string, ChatLayout> = {};
  const storedChats = isRecord(value.chats) ? value.chats : {};
  for (const [chatId, layout] of Object.entries(storedChats).slice(
    0,
    MAX_REMEMBERED_CHATS,
  )) {
    if (!chatId || !isRecord(layout)) continue;
    const surface = normalizeSurface(layout.surface);
    if (!opensBesideTranscript(surface.kind)) continue;
    chats[chatId] = { surface, expanded: layout.expanded === true };
  }

  return { fraction: clampFraction(value.fraction), chats };
}

/**
 * Record a chat's layout, keeping the most recently touched chats. A layout
 * that is back on the transcript is forgotten rather than stored, so the map
 * only holds chats with something open.
 */
export function rememberChatLayout(
  layouts: WorkspaceLayouts,
  chatId: string,
  layout: ChatLayout,
): WorkspaceLayouts {
  const chats = { ...layouts.chats };
  delete chats[chatId];

  if (opensBesideTranscript(layout.surface.kind)) {
    const ordered = Object.entries(chats).slice(0, MAX_REMEMBERED_CHATS - 1);
    return {
      fraction: layouts.fraction,
      chats: { [chatId]: layout, ...Object.fromEntries(ordered) },
    };
  }

  return { fraction: layouts.fraction, chats };
}

export function layoutForChat(
  layouts: WorkspaceLayouts,
  chatId: string | null,
): ChatLayout | null {
  if (!chatId) return null;
  return layouts.chats[chatId] ?? null;
}

export type WorkspaceSlots = {
  showTranscript: boolean;
  showPanel: boolean;
  /** Width share for the panel; only meaningful when both slots are shown. */
  fraction: number;
};

/**
 * Decide what the workspace body shows. A narrow window shows one surface at a
 * time rather than compressing both, which is also what expanding does on a
 * wide one.
 */
export function resolveSlots(input: {
  surface: Surface;
  expanded: boolean;
  fraction: number;
  narrow: boolean;
}): WorkspaceSlots {
  const fraction = clampFraction(input.fraction);
  if (!opensBesideTranscript(input.surface.kind)) {
    return { showTranscript: true, showPanel: false, fraction };
  }
  if (input.narrow || input.expanded) {
    return { showTranscript: false, showPanel: true, fraction };
  }
  return { showTranscript: true, showPanel: true, fraction };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
