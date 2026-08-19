// @vitest-environment jsdom

import { cleanup, render, screen, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { setThemeMode } from "@/theme";

const xlsxMocks = vi.hoisted(() => {
  const controller = {
    activeCellAddress: "B7",
    activeSheet: {
      colWidthOverridesPx: {},
      defaultColWidthPx: 64,
      defaultRowHeightPx: 20,
      hiddenCols: [],
      hiddenRows: [],
      rowHeightOverridesPx: {},
    },
    activeTabIndex: 0,
    canZoomIn: true,
    canZoomOut: true,
    error: null,
    isLoading: false,
    resetZoom: vi.fn(),
    selectRange: vi.fn(),
    selectedFormula: "=SUM(B2:B6)",
    selectedFormulaTarget: { kind: "cell", cell: { row: 6, col: 1 } },
    selectedRangeAddress: "B7:D9",
    selectedValue: "42",
    setActiveTabIndex: vi.fn(),
    tabs: [
      { id: "sheet-revenue", kind: "sheet", name: "Revenue", sheetIndex: 0 },
      { id: "sheet-costs", kind: "sheet", name: "Costs", sheetIndex: 1 },
    ],
    zoomIn: vi.fn(),
    zoomOut: vi.fn(),
    zoomScale: 100,
  };

  return {
    controller,
    setWasmSource: vi.fn(),
    useXlsxViewerController: vi.fn(() => controller),
    xlsxViewer: vi.fn(),
  };
});

vi.mock("@dukelib/sheets-wasm/duke_sheets_wasm_bg.wasm?url", () => ({
  default: "/assets/duke_sheets_wasm_bg.wasm",
}));

vi.mock("@extend-ai/react-xlsx", () => ({
  setWasmSource: xlsxMocks.setWasmSource,
  useXlsxViewerController: xlsxMocks.useXlsxViewerController,
  XlsxViewer: (props: { toolbar?: ReactNode }) => {
    xlsxMocks.xlsxViewer(props);
    return <div data-testid="xlsx-canvas">{props.toolbar}</div>;
  },
}));

vi.mock("@/document/useFileDownload", () => ({
  useFileDownload: () => ({
    data: new ArrayBuffer(8),
    error: null,
    isLoading: false,
    progress: null,
  }),
}));

import NativeSpreadsheetViewer from "./NativeSpreadsheetViewer";
import type { FileBytesSource } from "./useFileDownload";

const source: FileBytesSource = {
  id: "book-1",
  cacheKey: "output/book-1",
  fetch: vi.fn(),
};

describe("NativeSpreadsheetViewer", () => {
  beforeEach(() => {
    setThemeMode("light");
    xlsxMocks.useXlsxViewerController.mockClear();
    xlsxMocks.xlsxViewer.mockClear();
    xlsxMocks.controller.selectRange.mockClear();
    xlsxMocks.controller.setActiveTabIndex.mockClear();
  });
  afterEach(() => {
    cleanup();
    setThemeMode("system");
  });

  it("preserves cached Excel results and exposes a read-only canvas", async () => {
    render(
      <NativeSpreadsheetViewer
        source={source}
        mediaType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
      />,
    );

    expect(await screen.findByTestId("xlsx-canvas")).toBeInTheDocument();
    expect(xlsxMocks.useXlsxViewerController).toHaveBeenCalledWith(
      expect.objectContaining({
        fileName: "workbook.xlsx",
        maxFileSizeBytes: 16 * 1024 * 1024,
        readOnly: true,
        useWorker: true,
      }),
    );
    expect(xlsxMocks.xlsxViewer).toHaveBeenCalledWith(
      expect.objectContaining({
        allowResizeInReadOnly: false,
        getCellStyle: expect.any(Function),
        isDark: false,
        readOnly: true,
        showDefaultToolbar: false,
        showImages: true,
      }),
    );
    expect(screen.getByLabelText("Selected cell or range")).toHaveTextContent(
      "B7:D9",
    );
    expect(screen.getByLabelText("Cell value or formula")).toHaveTextContent(
      "=SUM(B2:B6)",
    );
    expect(screen.getByRole("tab", { name: "Revenue" })).toHaveAttribute(
      "aria-selected",
      "true",
    );

    const viewerProps = xlsxMocks.xlsxViewer.mock.lastCall?.[0] as {
      getCellStyle: (context: unknown) => React.CSSProperties | undefined;
    };
    const context = {
      cell: { row: 6, col: 0 },
      hasChartHighlight: false,
      hasConditionalFormat: false,
      hasHyperlink: false,
      hasValidation: false,
      isMerged: true,
      isTableHeader: false,
      resolvedStyle: { fontSize: "24px", textAlign: "left" },
      sheetName: "Executive Dashboard",
      value: "$17,493,399",
      workbookSheetIndex: 0,
    };
    expect(viewerProps.getCellStyle(context)).toEqual({ textAlign: "center" });
    expect(
      viewerProps.getCellStyle({ ...context, value: "Launch thesis" }),
    ).toBeUndefined();

    setThemeMode("dark");
    await waitFor(() =>
      expect(xlsxMocks.xlsxViewer).toHaveBeenLastCalledWith(
        expect.objectContaining({ isDark: true }),
      ),
    );
  });

  it("selects a cited A1 range on the cited sheet", async () => {
    render(
      <NativeSpreadsheetViewer
        source={source}
        mediaType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        highlightRange={{
          startCell: "C4",
          endCell: "E8",
          sheetName: "Revenue",
          sheetIndex: null,
        }}
      />,
    );

    await waitFor(() =>
      expect(xlsxMocks.controller.selectRange).toHaveBeenCalledWith({
        start: { row: 3, col: 2 },
        end: { row: 7, col: 4 },
      }),
    );
  });
});
