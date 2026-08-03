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
 * The grammar is `{type}` or `{type}.{id}`, with one document panel taking a
 * second identifier:
 *
 *   chat
 *   document.{documentId}
 *   document.{documentId}.{citationId}
 *   outputs
 *   outputs.{outputId}
 *   folders
 *   apps
 *   apps.{appId}
 *   agent.{runId}
 *
 * Only the first separator picks the panel type; what follows is read by that
 * panel and nothing else. Output navigation carries the durable opaque output
 * identity rather than a display filename. A source identifier is followed by
 * the citation to open it at, when the reader arrived from one.
 */
export function parsePanelSegment(segment: string): PanelContent | null {
  const separator = segment.indexOf(".");
  const type = separator === -1 ? segment : segment.slice(0, separator);
  const id = separator === -1 ? "" : segment.slice(separator + 1);

  switch (type) {
    case "chat":
      return id ? null : { type: "chat" };
    case "folders":
      return id ? null : { type: "folders" };
    case "document":
      return parseDocumentTarget(id);
    case "sources":
      // Historical links used `sources.{document}.{citation}`. Preserve those
      // detail links while refusing the retired bare catalog.
      return id ? parseDocumentTarget(id) : null;
    case "outputs":
      return id ? { type: "outputs", outputId: id } : { type: "outputs" };
    case "apps":
      return id ? { type: "apps", appId: id } : { type: "apps" };
    case "agent":
      // A run id is the whole address; there is no bare agent panel.
      return id ? { type: "agent", runId: id } : null;
    default:
      return null;
  }
}

function parseDocumentTarget(id: string): PanelContent | null {
  if (!id) return null;
  const separator = id.indexOf(".");
  if (separator === -1) return { type: "document", documentId: id };

  const documentId = id.slice(0, separator);
  const citationId = id.slice(separator + 1);
  // A citation is a position inside one document, so neither half addresses
  // anything alone, and a third segment is not part of the grammar.
  if (!documentId || !citationId || citationId.includes(".")) return null;
  return { type: "document", documentId, citationId };
}

export function encodePanelSegment(panel: PanelContent): string {
  switch (panel.type) {
    case "chat":
      return "chat";
    case "folders":
      return "folders";
    case "document":
      return panel.citationId
        ? `document.${panel.documentId}.${panel.citationId}`
        : `document.${panel.documentId}`;
    case "outputs":
      return panel.outputId ? `outputs.${panel.outputId}` : "outputs";
    case "apps":
      return panel.appId ? `apps.${panel.appId}` : "apps";
    case "agent":
      return `agent.${panel.runId}`;
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
