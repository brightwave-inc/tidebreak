import { useEffect, useRef, useState, type ReactNode } from "react";
import { Panel, PanelGroup, type ImperativePanelGroupHandle } from "react-resizable-panels";

import { cn } from "@/lib/utils";
import { PanelDragHandle } from "./PanelDragHandle";
import { PanelTabs } from "./PanelTabs";
import { activePanel, type LayoutState, type PanelContent } from "./panelTypes";
import { usePanelNav } from "./usePanelNav";

const MIN_PANEL_SIZE = 25;
const MAX_PANEL_SIZE = 75;

/**
 * Two regions: the conversation, and the panels open beside it.
 *
 * The conversation is always mounted and never displaced — a panel expanded
 * over it only takes the width, because its pollers for pending approvals and
 * questions have to keep running behind it.
 *
 * Sizes are driven imperatively rather than through `defaultSize`, because the
 * arrangement is a function of the URL and the regions have to follow it even
 * when the reader has since dragged them somewhere else. A hidden region takes
 * a dynamic `minSize` of zero instead of being `collapsible`, so a drag can
 * never squeeze one out of existence — only closing the tabs can.
 *
 * The whole arrangement is applied in one `setLayout` rather than resizing
 * each region in turn: sequential resizes are each clamped by the other's
 * current bounds, so an intermediate step can be rejected and leave the group
 * somewhere nobody asked for.
 */
export function PanelLayout({
  layout,
  tabLabel,
  renderChat,
  renderPanel,
  framed = true,
}: {
  layout: LayoutState;
  /** Name tabs after the things they show; see {@link PanelTabs}. */
  tabLabel?: (panel: PanelContent) => string | undefined;
  /**
   * `visible` is false while a panel is expanded over the conversation. The
   * conversation is rendered either way, so the body is told rather than
   * unmounted.
   */
  renderChat: (visible: boolean) => ReactNode;
  /** Only the tab showing is rendered; the rest are addresses, not mounts. */
  renderPanel: (panel: PanelContent) => ReactNode;
  /**
   * The card chrome. Chat relies on this group being the card. A host that
   * already frames the workspace — header, review rail, terminal drawer —
   * turns it off so the group is only the split.
   */
  framed?: boolean;
}) {
  const groupRef = useRef<ImperativePanelGroupHandle>(null);
  const [dragging, setDragging] = useState(false);
  const { focusTab, closeTab } = usePanelNav();

  const hasTabs = layout.tabs.length > 0;
  const fullscreen = hasTabs && layout.fullscreen;
  const panel = activePanel(layout);

  useEffect(() => {
    const group = groupRef.current;
    // Before the group has registered its regions there is no layout to
    // replace, and handing it one is rejected outright. The regions' own
    // defaults already describe the arrangement at that point.
    if (!group || group.getLayout().length === 0) return;
    group.setLayout(panelSizes({ hasTabs, fullscreen }));
  }, [hasTabs, fullscreen]);

  const initialSizesRef = useRef(panelSizes({ hasTabs, fullscreen }));
  const initialSizes = initialSizesRef.current;

  const showChat = !fullscreen;

  return (
    <PanelGroup
      ref={groupRef}
      direction="horizontal"
      className={cn(
        // min-w-0 stops the group's content-driven min-content width from
        // pushing the row wider than its flex basis, which is what let panel
        // content squeeze the sidebar rail beside it.
        "min-h-0 w-full max-w-full min-w-0 flex-1 overflow-clip",
        framed && "content-container",
      )}
      data-dragging={dragging || undefined}
    >
      <Panel
        order={1}
        minSize={showChat ? (hasTabs ? MIN_PANEL_SIZE : 100) : 0}
        maxSize={showChat ? (hasTabs ? MAX_PANEL_SIZE : 100) : 0}
        defaultSize={initialSizes[0]}
        className="panel-animated"
      >
        {renderChat(showChat)}
      </Panel>

      <PanelDragHandle disabled={!hasTabs || Boolean(fullscreen)} onDragging={setDragging} />

      <Panel
        order={2}
        minSize={hasTabs && !fullscreen ? MIN_PANEL_SIZE : 0}
        maxSize={hasTabs ? 100 : 0}
        defaultSize={initialSizes[1]}
        className="panel-animated"
      >
        {panel && (
          <div className="flex h-full w-full min-w-0 flex-col overflow-clip">
            <div className="shrink-0 px-1 pt-1">
              <PanelTabs
                tabs={layout.tabs}
                activeIndex={layout.activeIndex}
                labelFor={tabLabel}
                onSelect={focusTab}
                onClose={closeTab}
              />
            </div>
            <div className="flex min-h-0 flex-1 flex-col">{renderPanel(panel)}</div>
          </div>
        )}
      </Panel>
    </PanelGroup>
  );
}

/**
 * The share each region takes, as `[chat, panels]`.
 *
 * Nothing open leaves the conversation the window; anything open splits it,
 * and an expanded panel takes all of it.
 */
export function panelSizes({
  hasTabs,
  fullscreen,
}: {
  hasTabs: boolean;
  fullscreen?: boolean;
}): [number, number] {
  if (!hasTabs) return [100, 0];
  if (fullscreen) return [0, 100];
  return [50, 50];
}
