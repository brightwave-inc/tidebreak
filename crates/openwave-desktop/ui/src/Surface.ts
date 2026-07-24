/**
 * Where the chat workspace is pointed.
 *
 * A surface name alone cannot express "open this source at this citation" or
 * "open this output", so the selection is a descriptor: a surface, optionally
 * an item owned by that surface, and optionally a position within that item.
 * Identifiers are opaque and come from the API or the native catalog — never a
 * host path.
 */
export type SurfaceKind =
  | "chat"
  | "documents"
  | "deliverables"
  | "folders"
  | "settings";

export type Surface = {
  kind: SurfaceKind;
  itemId?: string;
  anchor?: string;
};

type TargetSupport = { item: boolean; anchor: boolean };

/**
 * What each surface can be pointed at. Surfaces gain targets as the views that
 * resolve them land, so this table is the single place that has to change.
 */
const SURFACE_TARGETS: Record<SurfaceKind, TargetSupport> = {
  chat: { item: false, anchor: false },
  documents: { item: true, anchor: true },
  deliverables: { item: true, anchor: false },
  folders: { item: false, anchor: false },
  settings: { item: false, anchor: false },
};

export const CHAT_SURFACE: Surface = { kind: "chat" };

export function isSurfaceKind(value: unknown): value is SurfaceKind {
  return typeof value === "string" && Object.hasOwn(SURFACE_TARGETS, value);
}

/**
 * Reduce an arbitrary value — a caller's argument, or a descriptor restored
 * from storage — to one the workspace can render. An unrecognised surface
 * falls back to the transcript. A target the surface does not accept, or an
 * anchor with no item to anchor to, is dropped so the surface opens on its own
 * list instead of an empty pane.
 */
export function normalizeSurface(value: unknown): Surface {
  if (!isRecord(value) || !isSurfaceKind(value.kind)) return CHAT_SURFACE;

  const kind = value.kind;
  const supports = SURFACE_TARGETS[kind];
  const itemId = supports.item ? nonEmptyString(value.itemId) : null;
  if (!itemId) return { kind };

  const anchor = supports.anchor ? nonEmptyString(value.anchor) : null;
  return anchor ? { kind, itemId, anchor } : { kind, itemId };
}

export function sameSurface(left: Surface, right: Surface): boolean {
  return (
    left.kind === right.kind &&
    left.itemId === right.itemId &&
    left.anchor === right.anchor
  );
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function nonEmptyString(value: unknown): string | null {
  if (typeof value !== "string") return null;
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : null;
}
