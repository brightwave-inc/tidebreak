import {
  areSamePanelType,
  type LayoutState,
  type PanelContent,
  type PanelPosition,
} from "./panelTypes";

/**
 * Panels are addressed by the URL, so a layout can be restored, gone back to,
 * and linked at — which is what a citation needs in order to point at a place
 * inside a document rather than just at the app.
 *
 * The grammar is `{type}` or `{type}.{id}`:
 *
 *   chat
 *   chats
 *   sources
 *   sources.{documentId}
 *   outputs
 *   outputs.{filename}
 *   folders
 *
 * Only the first separator is significant. Output filenames carry their own
 * dots — `outputs.report.md` is one output named `report.md`, not a malformed
 * segment — so the identifier is the whole remainder of the string.
 */
export function parsePanelSegment(segment: string): PanelContent | null {
  const separator = segment.indexOf(".");
  const type = separator === -1 ? segment : segment.slice(0, separator);
  const id = separator === -1 ? "" : segment.slice(separator + 1);

  switch (type) {
    case "chat":
      return id ? null : { type: "chat" };
    case "chats":
      return id ? null : { type: "chats" };
    case "folders":
      return id ? null : { type: "folders" };
    case "sources":
      return id ? { type: "sources", documentId: id } : { type: "sources" };
    case "outputs":
      return id ? { type: "outputs", filename: id } : { type: "outputs" };
    default:
      return null;
  }
}

export function encodePanelSegment(panel: PanelContent): string {
  switch (panel.type) {
    case "chat":
      return "chat";
    case "chats":
      return "chats";
    case "folders":
      return "folders";
    case "sources":
      return panel.documentId ? `sources.${panel.documentId}` : "sources";
    case "outputs":
      return panel.filename ? `outputs.${panel.filename}` : "outputs";
  }
}

export function parseFullscreenParam(value: string | undefined): PanelPosition | undefined {
  return value === "left" || value === "right" ? value : undefined;
}

/** The same panel on both sides is not a layout anyone asked for. */
export function isValidLayout(left: PanelContent, right: PanelContent): boolean {
  return !areSamePanelType(left, right);
}

export type PanelSearch = {
  left?: string;
  right?: string;
  fullscreen?: string;
};

/**
 * Read a layout out of the URL, falling back to the conversation alone whenever
 * the search params do not describe a usable one. A hand-edited or stale URL
 * should land the reader somewhere sensible rather than on an error.
 */
export function layoutFromSearch(search: PanelSearch): LayoutState {
  const single: LayoutState = { mode: "single", panel: { type: "chat" } };
  if (!search.left && !search.right) return single;

  const left = search.left ? parsePanelSegment(search.left) : { type: "chat" as const };
  const right = search.right ? parsePanelSegment(search.right) : { type: "chat" as const };
  if (!left || !right || !isValidLayout(left, right)) return single;
  if (left.type === "chat" && right.type === "chat") return single;

  return {
    mode: "split",
    left,
    right,
    fullscreen: parseFullscreenParam(search.fullscreen),
  };
}

/** The inverse of {@link layoutFromSearch}: `single` clears the params entirely. */
export function searchFromLayout(layout: LayoutState): PanelSearch {
  if (layout.mode === "single") {
    return { left: undefined, right: undefined, fullscreen: undefined };
  }
  return {
    left: encodePanelSegment(layout.left),
    right: encodePanelSegment(layout.right),
    fullscreen: layout.fullscreen,
  };
}
