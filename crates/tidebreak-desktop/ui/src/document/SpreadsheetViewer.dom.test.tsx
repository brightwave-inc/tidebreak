// @vitest-environment jsdom

import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const nativeViewerMock = vi.hoisted(() => vi.fn());

vi.mock("@/document/NativeSpreadsheetViewer", () => ({
  default: (props: unknown) => {
    nativeViewerMock(props);
    return <div>native workbook viewer</div>;
  },
}));

import { SpreadsheetViewer } from "./SpreadsheetViewer";
import type { FileBytesSource } from "./useFileDownload";

const source: FileBytesSource = {
  id: "book-1",
  cacheKey: "output/book-1",
  fetch: vi.fn(),
};

describe("SpreadsheetViewer", () => {
  beforeEach(() => nativeViewerMock.mockClear());
  afterEach(cleanup);

  it("routes XLSX directly to the native inspectable workbook surface", () => {
    render(
      <SpreadsheetViewer
        source={source}
        mediaType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        className="h-full"
      />,
    );

    expect(screen.getByText("native workbook viewer")).toBeInTheDocument();
    expect(nativeViewerMock).toHaveBeenCalledWith(
      expect.objectContaining({
        source,
        mediaType:
          "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        className: "h-full",
      }),
    );
  });

  it("forwards a cited workbook range to the same native surface", () => {
    const highlightRange = {
      startCell: "B7",
      endCell: "D9",
      sheetName: "Revenue",
      sheetIndex: null,
    };

    render(
      <SpreadsheetViewer
        source={source}
        mediaType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        highlightRange={highlightRange}
      />,
    );

    expect(nativeViewerMock).toHaveBeenCalledWith(
      expect.objectContaining({ highlightRange }),
    );
  });
});
