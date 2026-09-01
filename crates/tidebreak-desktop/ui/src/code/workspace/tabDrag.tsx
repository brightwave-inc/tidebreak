import {
  closestCenter,
  type CollisionDetection,
  PointerSensor,
  type PointerSensorOptions,
  pointerWithin,
  useDroppable,
} from "@dnd-kit/core";
import { EDITOR_SPLIT_DROP_ID, isEditorStripDropId } from "../editorDrag";
import type { PointerEvent as ReactPointerEvent } from "react";

/**
 * The pointer sensor, minus the controls that live inside a tab.
 *
 * A tab's close button sits within the draggable, so without this a press that
 * drifts a few pixels would pick the tab up rather than close it. A control
 * opts itself out by carrying the marker attribute.
 */
export class TabPointerSensor extends PointerSensor {
  static activators = [
    {
      eventName: "onPointerDown" as const,
      handler: (
        { nativeEvent: event }: ReactPointerEvent,
        { onActivation }: PointerSensorOptions,
      ) => {
        if (!event.isPrimary || event.button !== 0) return false;
        const target = event.target;
        if (!(target instanceof Element)) return true;
        if (target.closest('[data-no-drag="true"]')) return false;
        onActivation?.({ event });
        return true;
      },
    },
  ];
}

/**
 * The drop target under the pointer, with the nearest one as a fallback.
 *
 * A strip contains its tabs and overlaps the split zone, so it collides on
 * every drop that lands on either. Dropping it whenever something more specific
 * was hit is what makes a tab a reorder and the strip's open space an append.
 * The nearest-center fallback covers the frames where a fast drag has the
 * pointer outside every registered box.
 */
export const tabDropTarget: CollisionDetection = (args) => {
  const under = pointerWithin(args);
  const collisions = under.length > 0 ? under : closestCenter(args);
  const specific = collisions.filter(
    (collision) => !isEditorStripDropId(String(collision.id)),
  );
  return specific.length > 0 ? specific : collisions;
};

/** The mid-drag target that offers to open the tab beside the conversation. */
export function SplitDropZone() {
  const { isOver, setNodeRef } = useDroppable({ id: EDITOR_SPLIT_DROP_ID });
  return (
    <div
      ref={setNodeRef}
      data-testid="split-drop-zone"
      data-over={isOver ? "true" : undefined}
      className="workspace-split-drop-zone absolute inset-y-3 right-3 z-10 flex w-[min(40%,22rem)] flex-col items-center justify-center gap-2 rounded-xl border border-border bg-background/92 px-6 text-center shadow-lg backdrop-blur-md data-[over=true]:border-ring data-[over=true]:bg-accent/60"
    >
      <span className="grid size-9 place-items-center rounded-lg bg-muted text-foreground">
        <span className="grid grid-cols-2 gap-0.5" aria-hidden>
          <span className="h-4 w-2 rounded-[2px] bg-foreground/25" />
          <span className="h-4 w-2 rounded-[2px] bg-foreground" />
        </span>
      </span>
      <span className="text-sm font-semibold">Open beside the agent</span>
      <span className="max-w-44 text-xs leading-relaxed text-muted-foreground">
        Drop here to create a working pane on the right.
      </span>
    </div>
  );
}
