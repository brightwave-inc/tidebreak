import { listen } from "@tauri-apps/api/event";
import { useEffect, useState } from "react";
import { importDroppedLibraryDocuments } from "./documents";
import { hasNativeHost } from "./host";

type DropPhase = "enter" | "leave" | "dropped";

type DropState = {
  phase: DropPhase;
  accepted: boolean;
  fileCount: number;
};

export function DocumentDropTarget({ chatId }: { chatId: string }) {
  const [state, setState] = useState<DropState | null>(null);

  useEffect(() => {
    if (!hasNativeHost()) return;
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    void listen<unknown>("library-import-drop-state", (event) => {
      const next = parseDropState(event.payload);
      if (!next || cancelled) return;
      if (next.phase === "leave") {
        setState(null);
        return;
      }
      if (next.phase === "dropped") {
        setState(null);
        if (next.accepted) void importDroppedLibraryDocuments(chatId).catch(() => {});
        return;
      }
      setState(next);
    }).then((stop) => {
      if (cancelled) stop();
      else unlisten = stop;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [chatId]);

  if (!state) return null;
  return (
    <div
      className={`document-drop-target ${state.accepted ? "accept" : "reject"}`}
      aria-live="assertive"
    >
      <strong>
        {state.accepted
          ? `Add ${dropItemCountCopy(state.fileCount)} to this conversation`
          : "Only files and folders can be added as sources"}
      </strong>
      <span>
        {state.accepted
          ? "Release to add their documents in the background."
          : "Aliases and unavailable items cannot be imported."}
      </span>
    </div>
  );
}

export function parseDropState(value: unknown): DropState | null {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return null;
  const state = value as Record<string, unknown>;
  if (
    !["enter", "leave", "dropped"].includes(String(state.phase)) ||
    typeof state.accepted !== "boolean" ||
    typeof state.fileCount !== "number" ||
    !Number.isSafeInteger(state.fileCount) ||
    state.fileCount < 0
  ) {
    return null;
  }
  return {
    phase: state.phase as DropPhase,
    accepted: state.accepted,
    fileCount: state.fileCount,
  };
}

export function dropItemCountCopy(count: number): string {
  return count === 1 ? "this file or folder" : `${count} files or folders`;
}
