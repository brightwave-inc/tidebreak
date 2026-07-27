/**
 * The workspace is a pair of slots either side of the conversation, and a panel
 * is whatever is in one of them. Chat is a panel like any other rather than the
 * frame the others hang off — that is what lets a chat with a project and a
 * chat without one share a single layout.
 */
export type PanelContent =
  | { type: "chat" }
  | { type: "chats" }
  | { type: "sources"; documentId?: string }
  | { type: "outputs"; filename?: string }
  | { type: "folders" };

export type PanelType = PanelContent["type"];

export type PanelPosition = "left" | "right";

/**
 * Either the conversation alone, or two slots with the conversation possibly
 * squeezed out between them. `single` is the bare URL with no search params, so
 * the common case leaves no trail.
 */
export type LayoutState =
  | { mode: "single"; panel: PanelContent }
  | {
      mode: "split";
      left: PanelContent;
      right: PanelContent;
      fullscreen?: PanelPosition;
    };

export function areSamePanelType(a: PanelContent, b: PanelContent): boolean {
  return a.type === b.type;
}

/**
 * Navigation panels are lists you pick from; content panels are the thing you
 * picked. Navigation settles on the left, content on the right, and closing a
 * panel returns whatever is left to its own side rather than leaving it
 * stranded where the other panel happened to put it.
 */
export function isContentPanel(panel: PanelContent): boolean {
  switch (panel.type) {
    case "chat":
    case "chats":
    case "sources":
    case "outputs":
    case "folders":
      return false;
  }
}
