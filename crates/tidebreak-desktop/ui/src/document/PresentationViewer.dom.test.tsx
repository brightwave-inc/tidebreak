// @vitest-environment jsdom
import { act, cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

import {
  clearFileDownloadCache,
  type FileBytesSource,
} from "./useFileDownload";

const presentationMocks = vi.hoisted(() => ({
  pptxProps: null as Record<string, unknown> | null,
}));

vi.mock("@/components/extend/pptx-viewer", () => ({
  PptxViewerPreview: (props: Record<string, unknown>) => {
    presentationMocks.pptxProps = props;
    return <div data-testid="extend-pptx-viewer" />;
  },
}));

vi.mock("@/document/PdfViewer", () => ({
  PdfViewer: () => <div data-testid="converted-pdf-viewer" />,
}));

vi.mock("@/document/officePdf", () => {
  class ConverterMissingError extends Error {
    installable = false;
    installFailure = null;
  }
  class OfficeConversionError extends Error {
    details = "";
  }
  return {
    cancelPresentationConverterInstall: vi.fn(),
    ConverterMissingError,
    installPresentationConverter: vi.fn(() => Promise.resolve()),
    OfficeConversionError,
    officePdfSource: (source: FileBytesSource) => source,
  };
});

import { PresentationViewer } from "./PresentationViewer";

const PPTX =
  "application/vnd.openxmlformats-officedocument.presentationml.presentation";
const PPT = "application/vnd.ms-powerpoint";
const ODP = "application/vnd.oasis.opendocument.presentation";

function source(id = "deck-1"): FileBytesSource {
  return {
    id,
    cacheKey: `document/chat-1/${id}`,
    fetch: async () => ({
      bytes: new Uint8Array([0x50, 0x4b, 0x03, 0x04]),
      contentType: PPTX,
    }),
  };
}

beforeEach(() => {
  clearFileDownloadCache();
  presentationMocks.pptxProps = null;
  URL.createObjectURL = vi.fn(() => "blob:pptx-preview");
  URL.revokeObjectURL = vi.fn();
});

afterEach(cleanup);

it("renders PPTX directly with Extend and no upload or download actions", async () => {
  const { getByTestId } = render(
    <PresentationViewer source={source()} mediaType={PPTX} />,
  );

  await waitFor(() => expect(getByTestId("extend-pptx-viewer")).toBeTruthy());
  expect(presentationMocks.pptxProps).toMatchObject({
    defaultThumbnailSidebarOpen: true,
    showDownload: false,
    showToolbar: true,
    showUpload: false,
    src: "blob:pptx-preview",
  });
});

it.each([PPT, ODP])(
  "keeps %s on the LibreOffice compatibility path",
  async (mediaType) => {
    const { getByTestId, queryByTestId } = render(
      <PresentationViewer source={source(mediaType)} mediaType={mediaType} />,
    );

    await waitFor(() =>
      expect(getByTestId("converted-pdf-viewer")).toBeTruthy(),
    );
    expect(queryByTestId("extend-pptx-viewer")).toBeNull();
  },
);

it("falls back to LibreOffice when the direct PPTX renderer rejects a deck", async () => {
  const { getByTestId } = render(
    <PresentationViewer source={source()} mediaType={PPTX} />,
  );
  await waitFor(() => expect(getByTestId("extend-pptx-viewer")).toBeTruthy());

  const onError = presentationMocks.pptxProps?.onError as
    | (() => void)
    | undefined;
  act(() => onError?.());

  await waitFor(() => expect(getByTestId("converted-pdf-viewer")).toBeTruthy());
  expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:pptx-preview");
});
