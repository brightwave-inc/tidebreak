import type { CodeEditorRegion } from "./codeChrome";

/**
 * Private drag type for editor tabs.
 *
 * The previous split implementation only remembered the tab in React state.
 * A drop that arrived after that state had cleared looked exactly like an
 * unrelated drop and silently did nothing. The transfer itself is now the
 * source of truth; component state is only used to draw the preview.
 */
export const CODE_EDITOR_DRAG_TYPE = "application/x-tidebreak-editor-tab";
const CODE_EDITOR_DRAG_TEXT_PREFIX = "tidebreak-editor-tab";

export type EditorTabDrag = {
  region: CodeEditorRegion;
  index: number;
};

export function writeEditorTabDrag(
  transfer: DataTransfer,
  drag: EditorTabDrag,
): void {
  transfer.effectAllowed = "move";
  // Some desktop webviews reject or strip custom MIME payloads. Keep both
  // writes independent so either representation can carry the tab through.
  try {
    transfer.setData(CODE_EDITOR_DRAG_TYPE, JSON.stringify(drag));
  } catch {
    // The namespaced text payload below remains available.
  }
  try {
    transfer.setData(
      "text/plain",
      `${CODE_EDITOR_DRAG_TEXT_PREFIX}:${drag.region}:${drag.index}`,
    );
  } catch {
    // React drag state can still draw and handle the in-flight preview.
  }
}

export function hasEditorTabDrag(transfer: DataTransfer): boolean {
  const types = Array.from(transfer.types ?? []);
  return (
    types.includes(CODE_EDITOR_DRAG_TYPE) || types.includes("text/plain")
  );
}

export function readEditorTabDrag(
  transfer: Pick<DataTransfer, "getData">,
): EditorTabDrag | null {
  const encoded = readTransferData(transfer, CODE_EDITOR_DRAG_TYPE);
  if (encoded) {
    try {
      const parsed: unknown = JSON.parse(encoded);
      if (isEditorTabDrag(parsed)) return parsed;
    } catch {
      // Fall through to the compatibility payload below.
    }
  }

  const fallback = readTransferData(transfer, "text/plain");
  const match = new RegExp(
    `^${CODE_EDITOR_DRAG_TEXT_PREFIX}:(primary|secondary):(\\d+)$`,
  ).exec(fallback);
  if (!match) return null;
  return { region: match[1] as CodeEditorRegion, index: Number(match[2]) };
}

function readTransferData(
  transfer: Pick<DataTransfer, "getData">,
  type: string,
): string {
  try {
    return transfer.getData(type);
  } catch {
    return "";
  }
}

function isEditorTabDrag(value: unknown): value is EditorTabDrag {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Partial<EditorTabDrag>;
  return (
    (candidate.region === "primary" || candidate.region === "secondary") &&
    Number.isInteger(candidate.index) &&
    (candidate.index ?? -1) >= 0
  );
}
