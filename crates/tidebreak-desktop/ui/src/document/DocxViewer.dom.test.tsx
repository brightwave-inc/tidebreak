// @vitest-environment jsdom
import { cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

import { setThemeMode } from "@/theme";
import { clearFileDownloadCache, type FileBytesSource } from "./useFileDownload";

const docxMocks = vi.hoisted(() => ({
  props: null as Record<string, unknown> | null,
}));

vi.mock("@/components/extend/docx-viewer", () => ({
  DocxViewerPreview: (props: Record<string, unknown>) => {
    docxMocks.props = props;
    return <div data-testid="extend-docx-viewer" />;
  },
}));

import DocxViewer from "./DocxViewer";

const bytes = new Uint8Array([0x50, 0x4b, 0x03, 0x04]);

function source(id = "doc-1"): FileBytesSource {
  return {
    id,
    cacheKey: `document/chat-1/${id}`,
    fetch: async () => ({
      bytes,
      contentType:
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    }),
  };
}

beforeEach(() => {
  setThemeMode("light");
  clearFileDownloadCache();
  docxMocks.props = null;
  URL.createObjectURL = vi.fn(() => "blob:docx-preview");
  URL.revokeObjectURL = vi.fn();
});

afterEach(() => {
  cleanup();
  setThemeMode("system");
});

it("keeps the viewer read-only and follows live app theme changes", async () => {
  const { container, unmount } = render(<DocxViewer source={source()} />);

  await waitFor(() =>
    expect(container.querySelector('[data-testid="extend-docx-viewer"]')).toBeTruthy(),
  );
  expect(docxMocks.props).toMatchObject({
    isDark: false,
    showDownload: false,
    showToolbar: true,
    showUpload: false,
    src: "blob:docx-preview",
  });
  expect(container.firstElementChild).toHaveClass("bg-background");
  expect(URL.createObjectURL).toHaveBeenCalledTimes(1);

  setThemeMode("dark");
  await waitFor(() =>
    expect(docxMocks.props).toMatchObject({ isDark: true }),
  );
  expect(URL.createObjectURL).toHaveBeenCalledTimes(1);

  unmount();
  expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:docx-preview");
});
