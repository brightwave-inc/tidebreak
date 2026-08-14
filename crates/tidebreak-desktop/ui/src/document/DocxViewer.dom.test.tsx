// @vitest-environment jsdom
import { act, cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

import { clearFileDownloadCache, type FileBytesSource } from "./useFileDownload";

const docxMocks = vi.hoisted(() => ({
  renderAsync: vi.fn(),
}));

vi.mock("docx-preview", () => ({ renderAsync: docxMocks.renderAsync }));

import DocxViewer, { DOCX_RENDER_OPTIONS } from "./DocxViewer";

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

function renderPage(bodyContainer: HTMLElement, label = "document") {
  const hostileStyle = document.createElement("style");
  hostileStyle.textContent =
    "body { display: none !important; } </style><script>alert(1)</script><style>";
  bodyContainer.append(hostileStyle);

  const page = document.createElement("section");
  page.className = "docx";
  page.dataset.label = label;
  page.style.width = "816px";
  page.style.height = "1056px";
  Object.defineProperty(page, "offsetWidth", { value: 816 });

  const https = document.createElement("a");
  https.href = "https://example.com/report";
  https.textContent = "safe";
  page.append(https);

  const unsafe = document.createElement("a");
  unsafe.href = "http://example.com/plain";
  unsafe.textContent = "unsafe";
  page.append(unsafe);

  const bookmark = document.createElement("a");
  bookmark.href = "#section-2";
  bookmark.textContent = "bookmark";
  page.append(bookmark);

  bodyContainer.append(page);
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((res) => {
    resolve = res;
  });
  return { promise, resolve };
}

beforeEach(() => {
  clearFileDownloadCache();
  docxMocks.renderAsync.mockReset();
  docxMocks.renderAsync.mockImplementation(
    async (
      _data: ArrayBuffer,
      bodyContainer: HTMLElement,
      _styleContainer: HTMLElement,
    ) => {
      renderPage(bodyContainer);
      return {};
    },
  );
});
afterEach(cleanup);

it("confines generated document styles to an opaque-origin sandbox", async () => {
  const { container, rerender } = render(<DocxViewer source={source()} />);

  await waitFor(() => expect(docxMocks.renderAsync).toHaveBeenCalledTimes(1));
  expect(docxMocks.renderAsync).toHaveBeenCalledWith(
    expect.any(ArrayBuffer),
    expect.any(HTMLElement),
    expect.any(HTMLElement),
    DOCX_RENDER_OPTIONS,
  );

  const frame = await waitFor(() => {
    const candidate = container.querySelector("iframe");
    expect(candidate).not.toBeNull();
    return candidate as HTMLIFrameElement;
  });
  expect(frame).toHaveAttribute("sandbox", "allow-popups");
  expect(frame).not.toHaveAttribute(
    "sandbox",
    expect.stringContaining("allow-scripts"),
  );
  expect(frame).not.toHaveAttribute(
    "sandbox",
    expect.stringContaining("allow-same-origin"),
  );
  expect(frame).toHaveAttribute("referrerpolicy", "no-referrer");
  expect(frame.srcdoc).toContain("default-src 'none'; img-src data: blob:");
  expect(frame.srcdoc).toContain("script-src 'none'");
  expect(frame.srcdoc).toContain("connect-src 'none'");
  expect(frame.srcdoc).toContain("form-action 'none'");

  // docx-preview generated a document-global rule, but neither it nor the
  // rendered page ever entered Tidebreak's live document.
  expect(container.querySelector("section.docx")).toBeNull();
  expect(document.body.textContent).not.toContain(
    "body { display: none !important; }",
  );
  expect(frame.srcdoc).toContain("body { display: none !important; }");
  expect(frame.srcdoc).not.toContain("</style><script>alert(1)</script>");
  expect(frame.srcdoc).toContain('href="https://example.com/report"');
  expect(frame.srcdoc).toContain('target="_blank"');
  expect(frame.srcdoc).not.toContain("http://example.com/plain");

  rerender(<DocxViewer source={source()} />);
  expect(docxMocks.renderAsync).toHaveBeenCalledTimes(1);
});

it("does not let a superseded parse overwrite the current document", async () => {
  const firstRender = deferred<void>();
  docxMocks.renderAsync
    .mockImplementationOnce(
      async (
        _data: ArrayBuffer,
        bodyContainer: HTMLElement,
        _styleContainer: HTMLElement,
      ) => {
        await firstRender.promise;
        renderPage(bodyContainer, "old");
        return {};
      },
    )
    .mockImplementationOnce(
      async (
        _data: ArrayBuffer,
        bodyContainer: HTMLElement,
        _styleContainer: HTMLElement,
      ) => {
        renderPage(bodyContainer, "new");
        return {};
      },
    );

  const { container, rerender } = render(<DocxViewer source={source("old")} />);
  await waitFor(() => expect(docxMocks.renderAsync).toHaveBeenCalledTimes(1));

  rerender(<DocxViewer source={source("new")} />);
  await waitFor(() => expect(docxMocks.renderAsync).toHaveBeenCalledTimes(2));
  await waitFor(() => {
    expect(container.querySelector("iframe")?.getAttribute("srcdoc")).toContain(
      'data-label="new"',
    );
  });

  await act(async () => firstRender.resolve());
  expect(container.querySelector("iframe")?.getAttribute("srcdoc")).toContain(
    'data-label="new"',
  );
  expect(
    container.querySelector("iframe")?.getAttribute("srcdoc"),
  ).not.toContain('data-label="old"');
});
