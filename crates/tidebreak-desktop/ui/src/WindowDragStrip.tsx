import { hasMacOverlayTitlebar } from "./host";

/**
 * The draggable strip a full-window surface owes the macOS overlay titlebar.
 *
 * With the system chrome hidden, the window drags only where an element
 * carries `data-tauri-drag-region`. The shell's `Titlebar` provides that for
 * the normal chrome, but a surface that replaces the whole shell — a boot or
 * error screen, the managed sign-in gate — otherwise leaves the window
 * pinned. Rendered inside such a surface, this spans the strip the traffic
 * lights sit in; on other platforms it renders nothing.
 */
export function WindowDragStrip() {
  if (!hasMacOverlayTitlebar()) return null;
  return <div className="window-drag-strip" data-tauri-drag-region />;
}

/**
 * Props that let a pane header drag the window from its empty space.
 *
 * The shell's `Titlebar` covers only the rail, so the band across the top of
 * every pane beside it has to drag on its own. `deep` makes the whole header
 * a drag region except its controls: Tauri's drag script leaves buttons,
 * links, inputs, labels, and focusable elements clickable, and drags from
 * everything else, titles included, the way a native titlebar does. Spread
 * onto the header element. Off the macOS overlay titlebar it adds nothing.
 */
export function paneHeaderDragRegion(): {
  "data-tauri-drag-region"?: "deep";
} {
  return hasMacOverlayTitlebar() ? { "data-tauri-drag-region": "deep" } : {};
}

/**
 * The top band of a pane that has no header of its own to drag from.
 *
 * Render it as the first child of the pane's positioned scroll container: it
 * covers the empty padding at the top, and scrolls away with the page so it
 * never sits over content. Off the macOS overlay titlebar it renders nothing.
 */
export function PaneDragBand() {
  if (!hasMacOverlayTitlebar()) return null;
  return <div aria-hidden className="pane-drag-band" data-tauri-drag-region />;
}
