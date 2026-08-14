// @vitest-environment jsdom

import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@/document/PresentationViewer", () => ({
  ConvertedOfficeViewer: () => <div>high fidelity preview</div>,
}));

vi.mock("@/document/UniverSpreadsheetViewer", () => ({
  default: () => <div>interactive cell inspector</div>,
}));

import { SpreadsheetViewer } from "./SpreadsheetViewer";
import type { FileBytesSource } from "./useFileDownload";

const source: FileBytesSource = {
  id: "book-1",
  cacheKey: "output/book-1",
  fetch: vi.fn(),
};

describe("SpreadsheetViewer", () => {
  beforeEach(() => vi.clearAllMocks());
  afterEach(cleanup);

  it("opens on the rendered preview and lets the reader inspect cells", async () => {
    const user = userEvent.setup();
    render(
      <SpreadsheetViewer
        source={source}
        mediaType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
      />,
    );

    expect(screen.getByText("high fidelity preview")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Inspect cells" }));

    expect(screen.getByText("interactive cell inspector")).toBeInTheDocument();
  });

  it("uses the cell inspector when a citation names a workbook range", () => {
    render(
      <SpreadsheetViewer
        source={source}
        mediaType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        highlightRange={{
          startCell: "B7",
          endCell: "D9",
          sheetName: "Revenue",
          sheetIndex: null,
        }}
      />,
    );

    expect(screen.getByText("interactive cell inspector")).toBeInTheDocument();
  });
});
