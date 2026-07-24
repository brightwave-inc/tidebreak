/**
 * Where the chat workspace is pointed.
 *
 * A surface name alone cannot express "open this output", so the selection is a
 * descriptor: a surface plus, optionally, an item owned by that surface.
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
};

/**
 * Which surfaces can be pointed at an item, and so which ones have a view that
 * resolves one. A surface gains an entry when the view can act on it, not when
 * the descriptor could carry it: a target nothing consumes reads as a feature
 * and behaves as dead weight.
 */
const SURFACE_TARGETS: Record<SurfaceKind, { item: boolean }> = {
  chat: { item: false },
  documents: { item: false },
  deliverables: { item: true },
  folders: { item: false },
  settings: { item: false },
};

export const CHAT_SURFACE: Surface = { kind: "chat" };

function isSurfaceKind(value: unknown): value is SurfaceKind {
  return typeof value === "string" && Object.hasOwn(SURFACE_TARGETS, value);
}

/**
 * Reduce an arbitrary value — a caller's argument, or a descriptor restored
 * from storage — to one the workspace can render. An unrecognised surface
 * falls back to the transcript. A target the surface does not accept is
 * dropped so it opens on its own list instead of an empty pane.
 */
export function normalizeSurface(value: unknown): Surface {
  if (!isRecord(value) || !isSurfaceKind(value.kind)) return CHAT_SURFACE;

  const kind = value.kind;
  const itemId = SURFACE_TARGETS[kind].item
    ? nonEmptyString(value.itemId)
    : null;
  return itemId ? { kind, itemId } : { kind };
}

/** Whether two descriptors point at the same thing. */
export function sameSurface(left: Surface, right: Surface): boolean {
  return left.kind === right.kind && left.itemId === right.itemId;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function nonEmptyString(value: unknown): string | null {
  if (typeof value !== "string") return null;
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : null;
}
