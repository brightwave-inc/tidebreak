// @vitest-environment jsdom
import * as React from "react";
import { cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

import {
  clearFileDownloadCache,
  type FileBytesSource,
} from "./useFileDownload";

const pdfMocks = vi.hoisted(() => ({
  props: null as Record<string, unknown> | null,
  scrollToPage: vi.fn(),
}));

vi.mock("@/components/extend/pdf-viewer", async () => {
  const ReactModule = await import("react");
  const PDFViewer = ReactModule.forwardRef(
    (props: Record<string, unknown>, ref: React.ForwardedRef<unknown>) => {
      pdfMocks.props = props;
      ReactModule.useImperativeHandle(ref, () => ({
        getViewportElement: () => null,
        scrollToPage: pdfMocks.scrollToPage,
        scrollToPageArea: vi.fn(),
      }));
      ReactModule.useEffect(() => {
        const onDocumentLoadSuccess = props.onDocumentLoadSuccess as
          | ((pages: number) => void)
          | undefined;
        onDocumentLoadSuccess?.(6);
      }, [props.onDocumentLoadSuccess]);
      return <div data-testid="extend-pdf-viewer" />;
    },
  );
  return { PDFViewer };
});

import { PdfViewer } from "./PdfViewer";

function source(): FileBytesSource {
  return {
    id: "pdf-1",
    cacheKey: "document/chat-1/pdf-1",
    fetch: async () => ({
      bytes: new TextEncoder().encode("%PDF-1.7"),
      contentType: "application/pdf",
    }),
  };
}

beforeEach(() => {
  clearFileDownloadCache();
  sessionStorage.clear();
  pdfMocks.props = null;
  pdfMocks.scrollToPage.mockReset();
  URL.createObjectURL = vi.fn(() => "blob:pdf-preview");
  URL.revokeObjectURL = vi.fn();
});

afterEach(cleanup);

it("opens a PDF citation in Extend without viewer-owned file actions", async () => {
  const { unmount } = render(<PdfViewer source={source()} targetPage={4} />);

  await waitFor(() =>
    expect(pdfMocks.scrollToPage).toHaveBeenCalledWith(4, {
      behavior: "auto",
      block: "start",
    }),
  );
  expect(pdfMocks.props).toMatchObject({
    showDownload: false,
    showRotateControls: true,
    showToolbar: true,
    showUpload: false,
    src: "blob:pdf-preview",
  });
  expect(sessionStorage.getItem("pdf_page_pdf-1")).toBe("4");

  unmount();
  expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:pdf-preview");
});
