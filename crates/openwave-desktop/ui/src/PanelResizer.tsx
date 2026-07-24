import { useRef } from "react";
import { DEFAULT_FRACTION, MAX_FRACTION, MIN_FRACTION } from "./WorkspaceLayout";

const KEYBOARD_STEP = 0.05;

export type PanelResizerProps = {
  fraction: number;
  onFraction: (fraction: number) => void;
};

/**
 * The grip between the transcript and the side panel. Dragging is measured
 * against the workspace body rather than the pointer's delta, so a drag that
 * leaves the window and comes back does not accumulate drift.
 */
export function PanelResizer({ fraction, onFraction }: PanelResizerProps) {
  const gripRef = useRef<HTMLDivElement | null>(null);

  function fractionAt(clientX: number): number | null {
    const body = gripRef.current?.parentElement;
    if (!body) return null;
    const bounds = body.getBoundingClientRect();
    if (bounds.width <= 0) return null;
    return (bounds.right - clientX) / bounds.width;
  }

  return (
    <div
      ref={gripRef}
      role="separator"
      aria-orientation="vertical"
      aria-label="Resize panel"
      aria-valuemin={Math.round(MIN_FRACTION * 100)}
      aria-valuemax={Math.round(MAX_FRACTION * 100)}
      aria-valuenow={Math.round(fraction * 100)}
      tabIndex={0}
      className="panel-resizer"
      onPointerDown={(event) => {
        if (event.button !== 0) return;
        event.preventDefault();
        event.currentTarget.setPointerCapture(event.pointerId);
      }}
      onPointerMove={(event) => {
        if (!event.currentTarget.hasPointerCapture(event.pointerId)) return;
        const next = fractionAt(event.clientX);
        if (next !== null) onFraction(next);
      }}
      onPointerUp={(event) => {
        if (event.currentTarget.hasPointerCapture(event.pointerId)) {
          event.currentTarget.releasePointerCapture(event.pointerId);
        }
      }}
      onDoubleClick={() => onFraction(DEFAULT_FRACTION)}
      onKeyDown={(event) => {
        if (event.key === "ArrowLeft") {
          event.preventDefault();
          onFraction(fraction + KEYBOARD_STEP);
        } else if (event.key === "ArrowRight") {
          event.preventDefault();
          onFraction(fraction - KEYBOARD_STEP);
        }
      }}
    />
  );
}
