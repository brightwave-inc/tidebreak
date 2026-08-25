import {
  XlsxViewer,
  type XlsxCellRange,
  type XlsxCellStyleContext,
  type XlsxScrollerRenderProps,
  type XlsxSheetData,
  type XlsxViewerController,
  setWasmSource,
  useXlsxViewerController,
} from "@extend-ai/react-xlsx";
import workbookWasmUrl from "@dukelib/sheets-wasm/duke_sheets_wasm_bg.wasm?url";
import {
  ChevronLeftIcon,
  ChevronRightIcon,
  FunctionSquareIcon,
  MinusIcon,
  PlusIcon,
} from "lucide-react";
import {
  type HTMLAttributes,
  type MutableRefObject,
  type Ref,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";

import { FileDownloadProgressIndicator } from "@/components/document/FileDownloadProgress";
import { Button } from "@/components/ui/button";
import type { SheetHighlightRange } from "@/document/UniverSpreadsheetViewer";
import {
  projectWorkbookForReadOnlyDisplay,
  type ReadOnlyConditionalCellStyle,
  type ReadOnlyWorkbookProjection,
} from "@/document/readOnlyWorkbookProjection";
import {
  useFileDownload,
  type FileBytesSource,
} from "@/document/useFileDownload";
import { DocumentViewerState } from "@/document/ViewerPrimitives";
import { cn } from "@/lib/utils";
import { useTheme } from "@/theme";

// Make the parser asset an application resource. The viewer's default loader
// can discover its package-relative WASM in a browser build, but an explicit
// Vite asset URL also survives Tauri's production asset protocol and is passed
// into the worker without a network dependency.
setWasmSource(workbookWasmUrl);

interface Props extends HTMLAttributes<HTMLDivElement> {
  source: FileBytesSource;
  mediaType: string;
  highlightRange?: SheetHighlightRange;
}

const MAX_WORKBOOK_BYTES = 16 * 1024 * 1024;
const WORKBOOK_SURFACE = "bg-background text-foreground";
const LARGE_MERGED_VALUE_PATTERN =
  /^(?:[$€£¥]\s*)?\(?-?\d[\d,\s]*(?:\.\d+)?%?\)?$/;

/**
 * A native workbook surface: original OOXML is prepared as a read-only display
 * copy, parsed by a local WebAssembly engine, and drawn as a virtualized canvas
 * workbook. Nothing is flattened into PDF or reconstructed through the legacy
 * SheetJS/Univer path.
 */
export default function NativeSpreadsheetViewer({
  source,
  mediaType,
  highlightRange,
  className,
  ...restProps
}: Props) {
  const { resolved: resolvedTheme } = useTheme();
  const fileDownload = useFileDownload(source, { parseAs: "arrayBuffer" });

  if (fileDownload.isLoading) {
    return (
      <div
        className={cn(WORKBOOK_SURFACE, "relative min-h-0", className)}
        {...restProps}
      >
        {fileDownload.progress ? (
          <FileDownloadProgressIndicator progress={fileDownload.progress} />
        ) : (
          <DocumentViewerState variant="loading" className="h-full">
            Loading workbook…
          </DocumentViewerState>
        )}
      </div>
    );
  }

  if (fileDownload.error || fileDownload.data === null) {
    return (
      <div
        className={cn(WORKBOOK_SURFACE, "relative min-h-0", className)}
        {...restProps}
      >
        <DocumentViewerState variant="error" className="h-full">
          This workbook could not be loaded.
        </DocumentViewerState>
      </div>
    );
  }

  return (
    <LoadedWorkbook
      key={source.cacheKey}
      data={fileDownload.data}
      dark={resolvedTheme === "dark"}
      mediaType={mediaType}
      highlightRange={highlightRange}
      className={className}
      {...restProps}
    />
  );
}

interface LoadedWorkbookProps extends HTMLAttributes<HTMLDivElement> {
  data: ArrayBuffer;
  dark: boolean;
  mediaType: string;
  highlightRange?: SheetHighlightRange;
}

function LoadedWorkbook({
  data,
  dark,
  mediaType,
  highlightRange,
  className,
  ...restProps
}: LoadedWorkbookProps) {
  const projection = useReadOnlyProjection(data);

  if (projection.error) {
    return (
      <div
        className={cn(WORKBOOK_SURFACE, "relative min-h-0", className)}
        {...restProps}
      >
        <DocumentViewerState variant="error" className="h-full">
          This workbook could not be prepared.
        </DocumentViewerState>
      </div>
    );
  }

  if (!projection.value) {
    return (
      <div
        className={cn(WORKBOOK_SURFACE, "relative min-h-0", className)}
        {...restProps}
      >
        <DocumentViewerState variant="loading" className="h-full">
          Preparing workbook…
        </DocumentViewerState>
      </div>
    );
  }

  return (
    <RenderedWorkbook
      data={projection.value.data}
      dark={dark}
      conditionalStylesBySheet={projection.value.conditionalStylesBySheet}
      formulasBySheet={projection.value.formulasBySheet}
      mediaType={mediaType}
      highlightRange={highlightRange}
      className={className}
      {...restProps}
    />
  );
}

interface RenderedWorkbookProps extends LoadedWorkbookProps {
  conditionalStylesBySheet: Record<
    number,
    Record<string, ReadOnlyConditionalCellStyle>
  >;
  formulasBySheet: Record<number, Record<string, string>>;
}

function RenderedWorkbook({
  data,
  dark,
  conditionalStylesBySheet,
  formulasBySheet,
  mediaType,
  highlightRange,
  className,
  ...restProps
}: RenderedWorkbookProps) {
  const viewportRef = useRef<HTMLDivElement>(null);
  const controller = useXlsxViewerController({
    file: data,
    fileName: workbookFileName(mediaType),
    maxFileSizeBytes: MAX_WORKBOOK_BYTES,
    readOnly: true,
    useWorker: true,
  });

  useCitationNavigation(controller, viewportRef, highlightRange);

  const getCellStyle = useCallback(
    (context: XlsxCellStyleContext) =>
      readOnlyCellStyle(context, conditionalStylesBySheet),
    [conditionalStylesBySheet],
  );

  const renderScroller = useCallback(
    ({ children, viewportProps }: XlsxScrollerRenderProps) => {
      const { ref, ...props } = viewportProps;
      return (
        <div
          {...props}
          ref={(node) => {
            viewportRef.current = node;
            assignRef(ref, node);
          }}
        >
          {children}
        </div>
      );
    },
    [],
  );

  const toolbar = useMemo(
    () => (
      <WorkbookToolbar
        controller={controller}
        formulasBySheet={formulasBySheet}
      />
    ),
    [controller, formulasBySheet],
  );

  return (
    <div
      className={cn(WORKBOOK_SURFACE, "min-h-0 overflow-hidden", className)}
      {...restProps}
    >
      <XlsxViewer
        controller={controller}
        allowResizeInReadOnly={false}
        className="h-full min-h-0"
        enableCanvasSelectionAnimation
        enableGestureZoom
        errorState={(error) => (
          <DocumentViewerState variant="error" className="h-full">
            <span className="block">This workbook could not be read.</span>
            <span className="mt-1 block text-xs">{error.message}</span>
          </DocumentViewerState>
        )}
        experimentalCanvas
        getCellStyle={getCellStyle}
        isDark={dark}
        loadingState={
          <DocumentViewerState variant="loading" className="h-full">
            Reading workbook…
          </DocumentViewerState>
        }
        readOnly
        renderScroller={renderScroller}
        renderTableHeaderMenu={() => null}
        rounded={false}
        selectionColor="#0285ff"
        selectionFillColor="rgba(2, 133, 255, 0.10)"
        selectionHeaderColor="rgba(2, 133, 255, 0.16)"
        showDefaultToolbar={false}
        showImages
        toolbar={toolbar}
      />
    </div>
  );
}

function readOnlyCellStyle(
  {
    cell,
    isMerged,
    resolvedStyle,
    value,
    workbookSheetIndex,
  }: XlsxCellStyleContext,
  conditionalStylesBySheet: Record<
    number,
    Record<string, ReadOnlyConditionalCellStyle>
  >,
) {
  const projectedStyle =
    conditionalStylesBySheet[workbookSheetIndex]?.[cellAddress(cell)];
  const style: React.CSSProperties = {};

  if (projectedStyle?.backgroundColor) {
    style.backgroundColor = projectedStyle.backgroundColor;
  }
  if (projectedStyle?.dataBar) {
    const baseColor =
      typeof resolvedStyle.backgroundColor === "string" &&
      resolvedStyle.backgroundColor !== "transparent"
        ? resolvedStyle.backgroundColor
        : "#ffffff";
    const width = projectedStyle.dataBar.widthPercent;
    const startColor = tintHex(projectedStyle.dataBar.color, 0.1);
    const endColor = tintHex(projectedStyle.dataBar.color, 0.72);
    style.backgroundImage = `linear-gradient(90deg, ${startColor} 0%, ${endColor} ${width}%, ${baseColor} ${width}%, ${baseColor} 100%)`;
  }
  if (
    isMerged &&
    cssPixelValue(resolvedStyle.fontSize) >= 20 &&
    LARGE_MERGED_VALUE_PATTERN.test(value.trim())
  ) {
    style.textAlign = "center";
  }
  return Object.keys(style).length > 0 ? style : undefined;
}

function tintHex(color: string, amount: number): string {
  const match = color.match(/^#([0-9a-f]{6})$/i);
  if (!match?.[1]) return color;
  const ratio = Math.max(0, Math.min(1, amount));
  const channels = [0, 2, 4].map((index) =>
    Number.parseInt(match[1]!.slice(index, index + 2), 16),
  );
  return `rgb(${channels
    .map((channel) => Math.round(channel + (255 - channel) * ratio))
    .join(", ")})`;
}

function cssPixelValue(value: React.CSSProperties["fontSize"]): number {
  if (typeof value === "number") return value;
  if (typeof value !== "string") return 0;

  const parsed = Number.parseFloat(value);
  if (!Number.isFinite(parsed)) return 0;
  return value.trim().toLowerCase().endsWith("pt") ? (parsed * 4) / 3 : parsed;
}

function WorkbookToolbar({
  controller,
  formulasBySheet,
}: {
  controller: XlsxViewerController;
  formulasBySheet: Record<number, Record<string, string>>;
}) {
  const address =
    controller.selectedRangeAddress ?? controller.activeCellAddress ?? "";
  const originalFormula = selectedOriginalFormula(controller, formulasBySheet);
  const formula =
    originalFormula ??
    (controller.selectedFormula || controller.selectedValue || "");
  const selectedTab = controller.tabs[controller.activeTabIndex];

  return (
    <div className="shrink-0 border-b bg-background">
      <div className="flex h-10 min-w-0 items-center border-b">
        <output
          aria-label="Selected cell or range"
          className="flex h-full w-24 shrink-0 items-center border-r px-3 font-mono text-xs font-medium text-foreground"
        >
          {address || "—"}
        </output>
        <span
          aria-hidden="true"
          className="flex h-full w-10 shrink-0 items-center justify-center border-r text-muted-foreground"
        >
          <FunctionSquareIcon className="size-4" />
        </span>
        <output
          aria-label="Cell value or formula"
          className="min-w-0 grow truncate px-3 font-mono text-xs text-foreground"
          title={formula}
        >
          {formula}
        </output>
        <div className="flex h-full shrink-0 items-center gap-0.5 border-l px-2">
          <Button
            aria-label="Zoom out"
            disabled={!controller.canZoomOut}
            onClick={controller.zoomOut}
            size="icon-sm"
            variant="ghost"
          >
            <MinusIcon />
          </Button>
          <Button
            aria-label="Reset zoom"
            className="min-w-14 px-2 font-mono text-xs"
            onClick={controller.resetZoom}
            size="sm"
            variant="ghost"
          >
            {Math.round(controller.zoomScale)}%
          </Button>
          <Button
            aria-label="Zoom in"
            disabled={!controller.canZoomIn}
            onClick={controller.zoomIn}
            size="icon-sm"
            variant="ghost"
          >
            <PlusIcon />
          </Button>
        </div>
      </div>

      <div className="flex h-9 min-w-0 items-stretch bg-muted/30">
        <Button
          aria-label="Previous sheet"
          className="h-full rounded-none border-r px-2"
          disabled={controller.activeTabIndex <= 0}
          onClick={() =>
            controller.setActiveTabIndex(controller.activeTabIndex - 1)
          }
          size="icon-sm"
          variant="ghost"
        >
          <ChevronLeftIcon />
        </Button>
        <Button
          aria-label="Next sheet"
          className="h-full rounded-none border-r px-2"
          disabled={controller.activeTabIndex >= controller.tabs.length - 1}
          onClick={() =>
            controller.setActiveTabIndex(controller.activeTabIndex + 1)
          }
          size="icon-sm"
          variant="ghost"
        >
          <ChevronRightIcon />
        </Button>
        <div
          aria-label="Workbook sheets"
          className="flex min-w-0 grow items-stretch overflow-x-auto"
          role="tablist"
        >
          {controller.tabs.map((tab, index) => (
            <button
              aria-selected={index === controller.activeTabIndex}
              className={cn(
                "relative shrink-0 border-r px-3 text-xs font-medium text-muted-foreground transition-colors hover:bg-muted hover:text-foreground",
                index === controller.activeTabIndex &&
                  "bg-background text-foreground after:absolute after:inset-x-2 after:bottom-0 after:h-0.5 after:bg-[#0285ff]",
              )}
              key={tab.id}
              onClick={() => controller.setActiveTabIndex(index)}
              role="tab"
              title={tab.name}
              type="button"
            >
              {tab.name}
            </button>
          ))}
        </div>
        {selectedTab?.kind === "chartsheet" ? (
          <span className="flex shrink-0 items-center border-l px-3 text-xs text-muted-foreground">
            Chart sheet
          </span>
        ) : null}
      </div>
    </div>
  );
}

function useReadOnlyProjection(data: ArrayBuffer): {
  value: ReadOnlyWorkbookProjection | null;
  error: Error | null;
} {
  const [state, setState] = useState<{
    value: ReadOnlyWorkbookProjection | null;
    error: Error | null;
  }>({ value: null, error: null });

  useEffect(() => {
    let cancelled = false;
    setState({ value: null, error: null });
    void projectWorkbookForReadOnlyDisplay(data).then(
      (value) => {
        if (!cancelled) setState({ value, error: null });
      },
      (error: unknown) => {
        if (!cancelled) {
          setState({
            value: null,
            error: error instanceof Error ? error : new Error(String(error)),
          });
        }
      },
    );
    return () => {
      cancelled = true;
    };
  }, [data]);

  return state;
}

function selectedOriginalFormula(
  controller: XlsxViewerController,
  formulasBySheet: Record<number, Record<string, string>>,
): string | null {
  if (controller.selectedFormulaTarget?.kind !== "cell") return null;
  const cell = controller.selectedFormulaTarget.cell;
  const workbookSheetIndex = controller.activeSheet?.workbookSheetIndex;
  if (!cell || workbookSheetIndex === undefined) return null;
  return formulasBySheet[workbookSheetIndex]?.[cellAddress(cell)] ?? null;
}

function cellAddress(cell: { row: number; col: number }): string {
  let col = cell.col + 1;
  let letters = "";
  while (col > 0) {
    const remainder = (col - 1) % 26;
    letters = String.fromCharCode(65 + remainder) + letters;
    col = Math.floor((col - 1) / 26);
  }
  return `${letters}${cell.row + 1}`;
}

function useCitationNavigation(
  controller: XlsxViewerController,
  viewportRef: MutableRefObject<HTMLDivElement | null>,
  highlightRange: SheetHighlightRange | undefined,
) {
  const appliedRef = useRef<string | null>(null);
  const citation = useMemo(
    () => resolveCitation(controller, highlightRange),
    [controller.tabs, highlightRange],
  );

  useEffect(() => {
    if (!citation || controller.isLoading || controller.error) return;
    if (appliedRef.current === citation.key) return;

    if (
      citation.tabIndex !== null &&
      controller.activeTabIndex !== citation.tabIndex
    ) {
      controller.setActiveTabIndex(citation.tabIndex);
      return;
    }

    controller.selectRange(citation.range);
    appliedRef.current = citation.key;

    // Selection is controller state; scrolling belongs to the viewport. Wait
    // for the selected sheet and virtualized axes to commit before centering.
    let secondFrame = 0;
    const firstFrame = window.requestAnimationFrame(() => {
      secondFrame = window.requestAnimationFrame(() => {
        scrollRangeIntoView(
          viewportRef.current,
          controller.activeSheet,
          citation.range,
          controller.zoomScale,
        );
      });
    });
    return () => {
      window.cancelAnimationFrame(firstFrame);
      if (secondFrame) window.cancelAnimationFrame(secondFrame);
    };
  }, [citation, controller, viewportRef]);

  useEffect(() => {
    if (!highlightRange?.startCell) appliedRef.current = null;
  }, [highlightRange]);
}

function resolveCitation(
  controller: Pick<XlsxViewerController, "tabs">,
  highlightRange: SheetHighlightRange | undefined,
): { key: string; range: XlsxCellRange; tabIndex: number | null } | null {
  if (!highlightRange?.startCell) return null;
  const start = parseAddress(highlightRange.startCell);
  const end = parseAddress(highlightRange.endCell ?? highlightRange.startCell);
  if (!start || !end) return null;

  let tabIndex: number | null = null;
  if (highlightRange.sheetName) {
    const targetName = highlightRange.sheetName.toLocaleLowerCase();
    const found = controller.tabs.findIndex(
      (tab) =>
        tab.kind === "sheet" && tab.name.toLocaleLowerCase() === targetName,
    );
    if (found >= 0) tabIndex = found;
  } else if (
    highlightRange.sheetIndex !== null &&
    highlightRange.sheetIndex !== undefined
  ) {
    const found = controller.tabs.findIndex(
      (tab) =>
        tab.kind === "sheet" && tab.sheetIndex === highlightRange.sheetIndex,
    );
    if (found >= 0) tabIndex = found;
  }

  const range = { start, end };
  return {
    key: `${tabIndex ?? "active"}:${highlightRange.startCell}:${highlightRange.endCell ?? ""}`,
    range,
    tabIndex,
  };
}

function parseAddress(address: string): { row: number; col: number } | null {
  const match = address
    .trim()
    .replaceAll("$", "")
    .match(/^([A-Z]+)(\d+)$/i);
  if (!match?.[1] || !match[2]) return null;
  const row = Number.parseInt(match[2], 10) - 1;
  if (row < 0) return null;
  let col = 0;
  for (const letter of match[1].toUpperCase()) {
    col = col * 26 + letter.charCodeAt(0) - 64;
  }
  return { row, col: col - 1 };
}

function scrollRangeIntoView(
  viewport: HTMLDivElement | null,
  sheet: XlsxSheetData | null,
  range: XlsxCellRange,
  zoomScale: number,
) {
  if (!viewport || !sheet) return;
  const zoom = zoomScale / 100;
  const row = Math.min(range.start.row, range.end.row);
  const col = Math.min(range.start.col, range.end.col);
  const top = axisOffset(
    row,
    sheet.defaultRowHeightPx,
    sheet.rowHeightOverridesPx,
    sheet.hiddenRows,
  );
  const left = axisOffset(
    col,
    sheet.defaultColWidthPx,
    sheet.colWidthOverridesPx,
    sheet.hiddenCols,
  );
  viewport.scrollTop = Math.max(
    0,
    (24 + top) * zoom - viewport.clientHeight / 2,
  );
  viewport.scrollLeft = Math.max(
    0,
    (40 + left) * zoom - viewport.clientWidth / 2,
  );
  viewport.focus({ preventScroll: true });
}

function axisOffset(
  index: number,
  defaultSize: number,
  overrides: Record<number, number>,
  hidden: number[] | undefined,
): number {
  let offset = Math.max(0, index) * defaultSize;
  for (const [rawIndex, size] of Object.entries(overrides)) {
    const axisIndex = Number(rawIndex);
    if (axisIndex >= 0 && axisIndex < index) offset += size - defaultSize;
  }
  for (const axisIndex of hidden ?? []) {
    if (axisIndex >= 0 && axisIndex < index) {
      offset -= overrides[axisIndex] ?? defaultSize;
    }
  }
  return Math.max(0, offset);
}

function assignRef<T>(ref: Ref<T> | undefined, value: T | null) {
  if (typeof ref === "function") {
    ref(value);
  } else if (ref && "current" in ref) {
    ref.current = value;
  }
}

function workbookFileName(mediaType: string): string {
  return mediaType.split(";", 1)[0]!.trim().toLowerCase() ===
    "application/vnd.ms-excel"
    ? "workbook.xls"
    : "workbook.xlsx";
}
