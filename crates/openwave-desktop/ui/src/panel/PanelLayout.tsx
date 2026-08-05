import { useEffect, useRef, useState, type ReactNode } from "react";
import { Panel, PanelGroup, type ImperativePanelGroupHandle } from "react-resizable-panels";

import { cn } from "@/lib/utils";
import { PanelDragHandle } from "./PanelDragHandle";
import type { LayoutState, PanelContent } from "./panelTypes";

const MIN_PANEL_SIZE = 25;
const MAX_PANEL_SIZE = 75;

/**
 * Three slots — left, conversation, right — of which any may be empty.
 *
 * Sizes are driven imperatively rather than through `defaultSize`, because the
 * arrangement is a function of the URL and the panels have to follow it even
 * when the reader has since dragged them somewhere else. Panels take a dynamic
 * `minSize` of zero while hidden instead of being `collapsible`, so a drag can
 * never squeeze one out of existence — only closing it can.
 *
 * The whole arrangement is applied in one `setLayout` rather than resizing each
 * panel in turn: sequential resizes are each clamped by the others' current
 * bounds, so an intermediate step can be rejected and leave the group somewhere
 * nobody asked for.
 */
export function PanelLayout({
  layout,
  renderPanel,
}: {
  layout: LayoutState;
  /**
   * `visible` is false for a slot that has been sized out of the arrangement.
   * The conversation is rendered either way — its pollers for pending approvals
   * and questions have to keep running while a panel is expanded over it — so
   * the body is told rather than unmounted.
   */
  renderPanel: (
    panel: PanelContent,
    position: "left" | "right" | "chat",
    visible: boolean,
  ) => ReactNode;
}) {
  const groupRef = useRef<ImperativePanelGroupHandle>(null);
  const [dragging, setDragging] = useState(false);

  const isSplit = layout.mode === "split";
  const fullscreen = isSplit ? layout.fullscreen : undefined;
  const leftPanel = isSplit && layout.left.type !== "chat" ? layout.left : null;
  const rightPanel = isSplit && layout.right.type !== "chat" ? layout.right : null;
  const bothOpen = Boolean(leftPanel && rightPanel);

  useEffect(() => {
    const group = groupRef.current;
    // Before the group has registered its panels there is no layout to replace,
    // and handing it one is rejected outright. The panels' own defaults already
    // describe the arrangement at that point.
    if (!group || group.getLayout().length === 0) return;
    group.setLayout(
      panelSizes({ isSplit, fullscreen, hasLeft: Boolean(leftPanel), hasRight: Boolean(rightPanel) }),
    );
  }, [isSplit, fullscreen, leftPanel, rightPanel]);

  const initialSizesRef = useRef(
    panelSizes({ isSplit, fullscreen, hasLeft: Boolean(leftPanel), hasRight: Boolean(rightPanel) }),
  );
  const initialSizes = initialSizesRef.current;

  const showLeft = isSplit && Boolean(leftPanel);
  const showChat = !isSplit || (!fullscreen && !bothOpen);
  const showRight = isSplit && Boolean(rightPanel);

  return (
    <PanelGroup
      ref={groupRef}
      direction="horizontal"
      className={cn(
        // min-w-0 stops the group's content-driven min-content width from
        // pushing the row wider than its flex basis, which is what let panel
        // content squeeze the sidebar rail beside it.
        "content-container min-h-0 w-full max-w-full min-w-0 flex-1 overflow-clip",
      )}
      data-dragging={dragging || undefined}
    >
      <Panel
        order={1}
        minSize={showLeft && !fullscreen ? MIN_PANEL_SIZE : 0}
        maxSize={showLeft && !fullscreen ? MAX_PANEL_SIZE : 100}
        defaultSize={initialSizes[0]}
        className="panel-animated"
      >
        {showLeft && leftPanel && renderPanel(leftPanel, "left", true)}
      </Panel>

      <PanelDragHandle disabled={!showLeft || (!bothOpen && !showChat)} onDragging={setDragging} />

      <Panel
        order={2}
        minSize={showChat ? MIN_PANEL_SIZE : 0}
        maxSize={showChat ? 100 : 0}
        defaultSize={initialSizes[1]}
        className="panel-animated"
      >
        {renderPanel({ type: "chat" }, "chat", showChat)}
      </Panel>

      <PanelDragHandle disabled={!showRight || bothOpen || !showChat} onDragging={setDragging} />

      <Panel
        order={3}
        minSize={showRight && !fullscreen ? MIN_PANEL_SIZE : 0}
        maxSize={showRight && !fullscreen ? MAX_PANEL_SIZE : 100}
        defaultSize={initialSizes[2]}
        className="panel-animated"
      >
        {showRight && rightPanel && renderPanel(rightPanel, "right", true)}
      </Panel>
    </PanelGroup>
  );
}

/**
 * The share each slot takes, as `[left, chat, right]`.
 *
 * Two panels open leaves the conversation nothing: half a panel each is
 * readable, three columns is not.
 */
export function panelSizes({
  isSplit,
  fullscreen,
  hasLeft,
  hasRight,
}: {
  isSplit: boolean;
  fullscreen?: "left" | "right";
  hasLeft: boolean;
  hasRight: boolean;
}): [number, number, number] {
  if (!isSplit) return [0, 100, 0];
  if (fullscreen === "left") return [100, 0, 0];
  if (fullscreen === "right") return [0, 0, 100];
  if (hasLeft && hasRight) return [50, 0, 50];
  if (hasLeft) return [50, 50, 0];
  if (hasRight) return [0, 50, 50];
  return [0, 100, 0];
}
