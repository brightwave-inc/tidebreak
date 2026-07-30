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
