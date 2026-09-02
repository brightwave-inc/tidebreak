import type { DragEndEvent } from "@dnd-kit/core";
import type { LayoutState } from "@/panel/panelTypes";
import {
  type CodeEditorRegion,
  openCodeEditor,
  splitCodeChromeLayout,
} from "../codeChrome";
import { copyPlainText } from "@/ClipboardCopyButton";
import { dropEditorTab } from "../editorDrag";
import { toast } from "sonner";
import { useCodeUiStore } from "../CodeUiStore";
import { useEffect, useState } from "react";

/**
 * The file, diff, and drag state of the editor groups, plus the asks the
 * shell keymap raises for them.
 *
 * Quick open and the new-tab menu are requests counted upward: a listener
 * acts when the number changes, and the region named alongside says which
 * group the answer lands in. The shell raises each ask above the route, and
 * taking the flag here is what stops a remount from reopening a picker over
 * whatever the reader moved on to.
 */
export function useEditorTabs({
  layout,
  setLayout,
}: {
  layout: LayoutState;
  setLayout: (next: LayoutState) => void;
}) {
  const chrome = splitCodeChromeLayout(layout);
  const quickOpenPending = useCodeUiStore((state) => state.quickOpenPending);
  const newTabMenuPending = useCodeUiStore((state) => state.newTabMenuPending);
  const openFilePending = useCodeUiStore((state) => state.openFilePending);
  const [quickOpenRequest, setQuickOpenRequest] = useState(0);
  const [quickOpenTarget, setQuickOpenTarget] =
    useState<CodeEditorRegion>("primary");
  const [newTabMenuRequest, setNewTabMenuRequest] = useState(0);
  const [newTabMenuRegion, setNewTabMenuRegion] =
    useState<CodeEditorRegion>("primary");
  const [draggedTabId, setDraggedTabId] = useState<string | null>(null);
  const [fileReveal, setFileReveal] = useState<{
    path: string;
    line: number;
    revision: number;
  } | null>(null);

  function openTurnDiff(turnId: string) {
    setLayout(openCodeEditor(layout, { type: "diff", turnId }));
  }

  function openFile(
    path: string,
    line?: number,
    preferredRegion?: CodeEditorRegion,
  ) {
    setFileReveal((current) =>
      line === undefined
        ? null
        : {
            path,
            line,
            revision: (current?.revision ?? 0) + 1,
          },
    );
    setLayout(openCodeEditor(layout, { type: "file", path }, preferredRegion));
  }

  function openFileDiff(path: string) {
    setLayout(openCodeEditor(layout, { type: "diff", path }));
  }

  function requestNewTab(region: CodeEditorRegion) {
    setQuickOpenTarget(region);
    setQuickOpenRequest((request) => request + 1);
  }

  function showNewTabMenu(region: CodeEditorRegion) {
    setNewTabMenuRegion(region);
    setNewTabMenuRequest((request) => request + 1);
  }

  function finishTabDrag(event: DragEndEvent) {
    setDraggedTabId(null);
    const next = dropEditorTab(
      layout,
      String(event.active.id),
      event.over ? String(event.over.id) : null,
    );
    if (next) setLayout(next);
  }

  function copyEditorPath(path: string) {
    void copyPlainText(path)
      .then(() => toast.success("Copied path"))
      .catch(() => toast.error("Could not copy path"));
  }

  const splitFocused =
    Boolean(layout.editorSplit?.focused) && chrome.splitEditors.tabs.length > 0;

  useEffect(() => {
    if (!quickOpenPending) return;
    if (!useCodeUiStore.getState().takeQuickOpen()) return;
    requestNewTab(splitFocused ? "secondary" : "primary");
  }, [quickOpenPending, splitFocused]);

  useEffect(() => {
    if (!newTabMenuPending) return;
    if (!useCodeUiStore.getState().takeNewTabMenu()) return;
    showNewTabMenu(splitFocused ? "secondary" : "primary");
  }, [newTabMenuPending, splitFocused]);

  // The palette ranks worktree files but has nowhere to put one; the tabs live
  // here, so it names a path and this opens it.
  useEffect(() => {
    if (!openFilePending) return;
    const path = useCodeUiStore.getState().takeOpenFilePath();
    if (path) openFile(path);
  }, [openFilePending]);

  return {
    fileReveal,
    openFile,
    openTurnDiff,
    openFileDiff,
    quickOpenRequest,
    quickOpenTarget,
    newTabMenuRequest,
    newTabMenuRegion,
    requestNewTab,
    draggedTabId,
    setDraggedTabId,
    finishTabDrag,
    copyEditorPath,
    splitFocused,
  };
}
