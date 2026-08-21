import { ICommandService } from "@univerjs/core";
import {
  CalculationMode,
  type ISequenceNode,
  sequenceNodeType,
  UniverSheetsCorePreset,
} from "@univerjs/preset-sheets-core";
import sheetsCoreEnUS from "@univerjs/preset-sheets-core/locales/en-US";
import "@univerjs/preset-sheets-core/lib/index.css";
import type { FUniver, IWorkbookData, Univer } from "@univerjs/presets";
import { createUniver, LocaleType } from "@univerjs/presets";
import { Loader2Icon } from "lucide-react";
import type { HTMLAttributes, ReactNode } from "react";
import { useCallback, useEffect, useRef, useState } from "react";

import { cn } from "@/lib/utils";
import { useTheme } from "@/theme";
import UniverFormulaWorker from "@/workers/univer-formula.worker?worker&inline";
import { SpreadsheetShortcutsInfoBar } from "./SpreadsheetShortcutsInfo";
import { parseCellAddress } from "./spreadsheet";
import { FileDownloadProgressIndicator } from "@/components/document/FileDownloadProgress";
import { useFileDownload, type FileBytesSource } from "./useFileDownload";
import { useUniverWorker } from "./useUniverWorker";

/** A cell range a citation points at, resolved against a named or indexed sheet. */
export interface SheetHighlightRange {
  startCell: string | null | undefined;
  endCell: string | null | undefined;
  sheetName: string | null | undefined;
  sheetIndex: number | null | undefined;
}

interface Props extends HTMLAttributes<HTMLDivElement> {
  source: FileBytesSource;
  highlightRange?: SheetHighlightRange;
  /** When true, skip the XLSX-only style pass — a CSV has none. */
  isCsv?: boolean;
}

/** Mutable state for a mounted Univer instance, stored in a single ref. */
interface UniverInstance {
  core: Univer;
  api: FUniver;
  workbookData: IWorkbookData;
  canvasReady: boolean;
}

/**
 * The spreadsheet viewer: an xlsx, xls, or csv rendered with its sheet tabs,
 * formulas, and cell selection intact.
 *
 * Read-only. Univer is a full editor, so rather than trusting the absence of a
 * toolbar the viewer cancels every editing command before it executes, letting
 * navigation, selection, and the formula engine's own writes through.
 */
export default function UniverSpreadsheetViewer({
  source,
  highlightRange,
  isCsv,
  className,
  ...restProps
}: Props) {
  const containerRef = useRef<HTMLDivElement>(null);
  const univerInstanceRef = useRef<UniverInstance | null>(null);
  const highlightRangeRef = useRef(highlightRange);
  highlightRangeRef.current = highlightRange;
  const [errorType, setErrorType] = useState<"parse" | "load" | null>(null);
  // Destructured because the hook returns a fresh object every render; the
  // callbacks inside are stable, and only those may feed the init effect below
  // — depending on the object itself would tear Univer down and remount it on
  // every parent re-render (every composer keystroke, in practice).
  const { parseWorkbook, isProcessing } = useUniverWorker();
  const { resolved: resolvedTheme } = useTheme();
  const resolvedThemeRef = useRef(resolvedTheme);
  resolvedThemeRef.current = resolvedTheme;
  const fileId = source.id;

  // Sync dark mode with Univer.
  useEffect(() => {
    univerInstanceRef.current?.api.toggleDarkMode(resolvedTheme === "dark");
  }, [resolvedTheme]);

  // Keep a trackpad horizontal swipe from triggering back/forward navigation
  // while the spreadsheet is mounted. The webview processes that gesture at the
  // compositor level, before JS wheel events, so the only reliable opt-out is
  // overscroll-behavior-x on the root element.
  useEffect(() => {
    const html = document.documentElement;
    const previous = html.style.overscrollBehaviorX;
    html.style.overscrollBehaviorX = "none";
    return () => {
      html.style.overscrollBehaviorX = previous;
    };
  }, []);

  const fileDownload = useFileDownload(source, {
    parseAs: "arrayBuffer",
  });

  // Shortcuts the canvas does not handle itself.
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    const handleKeyDown = (e: KeyboardEvent) => {
      const instance = univerInstanceRef.current;
      if (!instance) return;
      const univerAPI = instance.api;

      // --- Trace precedents: Ctrl+[ ---
      if (e.ctrlKey && e.key === "[") {
        const workbook = univerAPI.getActiveWorkbook();
        const sheet = workbook?.getActiveSheet();
        if (!workbook || !sheet) return;

        const range = sheet.getSelection()?.getActiveRange();
        if (!range) return;

        const formula = sheet
          .getRange(range.getRow(), range.getColumn())
          .getFormula();
        if (!formula) return;

        const nodes = univerAPI.getFormula().sequenceNodesBuilder(formula);
        if (!nodes) return;

        const refs = nodes.filter(
          (n): n is ISequenceNode =>
            typeof n !== "string" && n.nodeType === sequenceNodeType.REFERENCE,
        );
        if (refs.length === 0) return;

        e.preventDefault();
        e.stopPropagation();

        const { token } = refs[0]!;

        let targetSheetName: string | null = null;
        let cellRef = token;
        const bangIdx = token.indexOf("!");
        if (bangIdx !== -1) {
          targetSheetName = token
            .substring(0, bangIdx)
            .replace(/^'|'$/g, "")
            .replace(/''/g, "'");
          cellRef = token.substring(bangIdx + 1);
        }

        const colonIdx = cellRef.indexOf(":");
        if (colonIdx !== -1) cellRef = cellRef.substring(0, colonIdx);
        cellRef = cellRef.replace(/\$/g, "");

        const target = parseCellAddress(cellRef);
        if (!target) return;

        if (targetSheetName) {
          workbook.getSheetByName(targetSheetName)?.activate();
        }

        const activeSheet = workbook.getActiveSheet();
        if (!activeSheet) return;

        activeSheet.getRange(target.row, target.col).activate();
        activeSheet.scrollToCell(
          Math.max(0, target.row - 5),
          Math.max(0, target.col - 2),
          200,
        );
        return;
      }

      // --- Switch sheet tabs: Ctrl+PageDown / Ctrl+PageUp ---
      if (e.ctrlKey && (e.key === "PageDown" || e.key === "PageUp")) {
        e.preventDefault();
        e.stopPropagation();

        const workbook = univerAPI.getActiveWorkbook();
        if (!workbook) return;

        const sheets = workbook.getSheets();
        const activeSheet = workbook.getActiveSheet();
        if (!activeSheet || sheets.length <= 1) return;

        const currentIdx = sheets.findIndex(
          (s) => s.getSheetId() === activeSheet.getSheetId(),
        );
        if (currentIdx === -1) return;

        const nextIdx =
          e.key === "PageDown"
            ? Math.min(currentIdx + 1, sheets.length - 1)
            : Math.max(currentIdx - 1, 0);

        if (nextIdx !== currentIdx) sheets[nextIdx]!.activate();
      }
    };

    container.addEventListener("keydown", handleKeyDown, { capture: true });
    return () => {
      container.removeEventListener("keydown", handleKeyDown, {
        capture: true,
      });
    };
  }, [fileDownload.data]);

  // Parse the workbook in the worker, then mount Univer on the result.
  useEffect(() => {
    if (!fileDownload.data || !containerRef.current) return;

    setErrorType(null);
    let disposed = false;

    parseWorkbook(fileDownload.data, fileId, { isCsv })
      .then(({ workbookData }) => {
        if (disposed || !containerRef.current) return;

        const corePreset = UniverSheetsCorePreset({
          container: containerRef.current,
          toolbar: false,
          contextMenu: false,
          workerURL: new UniverFormulaWorker(),
          formula: { initialFormulaComputing: CalculationMode.WHEN_EMPTY },
          sheets: {
            disableForceStringAlert: true,
            disableForceStringMark: true,
          },
          footer: { zoomSlider: false },
        });

        const { univer, univerAPI } = createUniver({
          locale: LocaleType.EN_US,
          locales: { [LocaleType.EN_US]: sheetsCoreEnUS },
          presets: [corePreset],
        });

        univerInstanceRef.current = {
          core: univer,
          api: univerAPI,
          workbookData,
          canvasReady: false,
        };
        univerAPI.toggleDarkMode(resolvedThemeRef.current === "dark");
        univerAPI.createWorkbook(workbookData);

        // Disable editing while preserving keyboard navigation: block the cell
        // editor from opening and block text insertion, but leave
        // set-activate-cell-edit alone because arrow-key navigation uses it
        // internally. The formula engine writes its computed values through
        // mutations, so those are let through by name.
        const formulaAllowlist = new Set([
          "sheet.mutation.set-range-values",
          "sheet.mutation.toggle-gridlines",
          "sheet.mutation.set-worksheet-col-width",
          "sheet.mutation.set-worksheet-col-auto-width",
          "sheet.mutation.set-worksheet-row-height",
          "sheet.mutation.set-worksheet-row-auto-height",
        ]);
        univerAPI.addEvent(univerAPI.Event.BeforeCommandExecute, (event) => {
          if (
            event.id === "sheet.operation.set-cell-edit-visible" ||
            event.id === "sheet.operation.set-cell-edit-visible-arrow" ||
            event.id === "doc.command.insert-text" ||
            event.id === "sheet.command.set-range-values" ||
            (event.id.startsWith("sheet.mutation.") &&
              !formulaAllowlist.has(event.id))
          ) {
            event.cancel = true;
          }
        });

        // Univer creates its canvas asynchronously, and its appearance is the
        // most reliable "the renderer is up" signal available. Poll for it, and
        // use it to gate a highlight that arrived before the mount finished.
        const MAX_CANVAS_POLL_ATTEMPTS = 25; // ~5s total
        window.setTimeout(() => {
          if (disposed || !containerRef.current) return;
          let attempts = 0;
          const findSpreadsheetCanvas = () => {
            if (disposed || !containerRef.current) return;
            attempts++;

            const canvases = containerRef.current.querySelectorAll("canvas");
            let bigCanvas: HTMLCanvasElement | null = null;
            canvases.forEach((c) => {
              if (c.offsetWidth > 100 && c.offsetHeight > 100) bigCanvas = c;
            });

            if (bigCanvas) {
              const instance = univerInstanceRef.current;
              if (instance) {
                instance.canvasReady = true;
                const pending = highlightRangeRef.current;
                if (pending?.startCell) applyHighlight(instance, pending);
              }
            } else if (attempts < MAX_CANVAS_POLL_ATTEMPTS) {
              window.setTimeout(findSpreadsheetCanvas, 200);
            }
          };
          findSpreadsheetCanvas();
        }, 100);
      })
      .catch(() => {
        if (!disposed) setErrorType("parse");
      });

    return () => {
      disposed = true;
      // Dispose both the core runtime and the facade so the formula engine's
      // state cannot leak into the next workbook opened in this window.
      if (univerInstanceRef.current) {
        univerInstanceRef.current.core.dispose();
        univerInstanceRef.current.api.dispose();
        univerInstanceRef.current = null;
      }
    };
  }, [fileDownload.data, fileId, isCsv, parseWorkbook]);

  // Apply a highlight that arrives after the canvas is up; one that arrives
  // before it is applied by the canvas poll above.
  useEffect(() => {
    const instance = univerInstanceRef.current;
    if (!instance || !instance.canvasReady || !highlightRange) return;
    applyHighlight(instance, highlightRange);
  }, [highlightRange]);

  useEffect(() => {
    if (fileDownload.error) setErrorType("load");
  }, [fileDownload.error]);

  const handleAutofit = useCallback(() => {
    const instance = univerInstanceRef.current;
    if (!instance) return;

    const sheet = instance.api.getActiveWorkbook()?.getActiveSheet();
    if (!sheet) return;

    const maxRow = sheet.getLastRow();
    const maxCol = sheet.getLastColumn();
    if (maxRow <= 0 || maxCol <= 0) return;

    const ranges = [
      {
        startRow: 0,
        endRow: maxRow - 1,
        startColumn: 0,
        endColumn: maxCol - 1,
      },
    ];

    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const injector = (instance.api as any)._injector;
    const commandService = injector.get(ICommandService) as ICommandService;

    const colCmd = "sheet.command.set-col-is-auto-width";
    const rowCmd = "sheet.command.set-row-is-auto-height";
    if (
      !commandService.hasCommand(colCmd) ||
      !commandService.hasCommand(rowCmd)
    ) {
      return;
    }

    void commandService.executeCommand(colCmd, { ranges });
    void commandService.executeCommand(rowCmd, { ranges });
  }, []);

  if (fileDownload.isLoading) {
    return (
      <div className={cn("relative overflow-auto", className)} {...restProps}>
        {fileDownload.progress ? (
          <FileDownloadProgressIndicator progress={fileDownload.progress} />
        ) : (
          <ViewerMessage spinner>Loading spreadsheet…</ViewerMessage>
        )}
      </div>
    );
  }

  if (errorType) {
    return (
      <div className={cn("relative overflow-auto", className)} {...restProps}>
        <ViewerMessage>
          {errorType === "parse"
            ? "This spreadsheet could not be read."
            : "This spreadsheet could not be loaded."}
        </ViewerMessage>
      </div>
    );
  }

  return (
    <div className={cn("relative flex flex-col", className)} {...restProps}>
      {isProcessing && (
        <div className="bg-background/80 absolute inset-0 z-10 flex items-center justify-center">
          <ViewerMessage spinner>Reading spreadsheet…</ViewerMessage>
        </div>
      )}
      <SpreadsheetShortcutsInfoBar onAutofit={handleAutofit} />
      <style>{`
        [data-u-comp="slide-tab-item"] {
            color: var(--muted-foreground) !important;
            background-color: transparent !important;
            font-weight: 500 !important;
            box-shadow: none !important;
        }
        [data-u-comp="slide-tab-item"]:hover {
            background-color: var(--muted) !important;
        }
        [data-u-comp="slide-tab-item"].univer-bg-white,
        [data-u-comp="slide-tab-item"].univer-font-bold {
            color: var(--foreground) !important;
            background-color: var(--muted) !important;
            font-weight: 700 !important;
        }
        .univerjs-icon-dropdown-icon {
            color: var(--muted-foreground) !important;
        }
      `}</style>
      <div
        ref={containerRef}
        className="min-h-0 grow"
        style={{ width: "100%", height: "100%" }}
      />
    </div>
  );
}

function ViewerMessage({
  spinner,
  children,
}: {
  spinner?: boolean;
  children: ReactNode;
}) {
  return (
    <div className="text-muted-foreground flex h-64 items-center justify-center">
      <div className="flex flex-col items-center gap-2">
        {spinner && <Loader2Icon className="size-6 animate-spin" />}
        <p>{children}</p>
      </div>
    </div>
  );
}

function applyHighlight(
  instance: UniverInstance,
  highlightRange: SheetHighlightRange,
) {
  const { api: univerAPI, workbookData } = instance;
  const activeWorkbook = univerAPI.getActiveWorkbook();
  if (!activeWorkbook) return;

  let targetSheetName = highlightRange.sheetName;
  if (
    !targetSheetName &&
    highlightRange.sheetIndex !== null &&
    highlightRange.sheetIndex !== undefined
  ) {
    const sheetId = workbookData.sheetOrder[highlightRange.sheetIndex];
    if (sheetId) targetSheetName = workbookData.sheets[sheetId]?.name ?? null;
  }

  if (targetSheetName) {
    activeWorkbook.getSheetByName(targetSheetName)?.activate();
  }

  if (!highlightRange.startCell) return;
  const start = parseCellAddress(highlightRange.startCell);
  if (!start) return;
  const end = highlightRange.endCell
    ? parseCellAddress(highlightRange.endCell)
    : start;
  if (!end) return;

  const activeSheet = activeWorkbook.getActiveSheet();
  if (!activeSheet) return;

  activeSheet
    .getRange(
      start.row,
      start.col,
      end.row - start.row + 1,
      end.col - start.col + 1,
    )
    .activate();

  // Offset the scroll target so the cell lands roughly in the middle of the
  // viewport instead of in the top-left corner.
  activeSheet.scrollToCell(
    Math.max(0, start.row - 5),
    Math.max(0, start.col - 2),
    200,
  );
}
