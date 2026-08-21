import { beforeEach, describe, expect, it, vi } from "vitest";

const invokeMock = vi.hoisted(() => vi.fn());
const listenMock = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));
vi.mock("@tauri-apps/api/event", () => ({ listen: listenMock }));

import {
  ConverterMissingError,
  convertOfficeToPdf,
  installPresentationConverter,
  officePdfSource,
  OfficeConversionError,
  resetConverterInstallStateForTest,
  warmPresentationConverter,
} from "./officePdf";
import type { FileBytesSource } from "./useFileDownload";

const PPTX =
  "application/vnd.openxmlformats-officedocument.presentationml.presentation";

beforeEach(() => {
  invokeMock.mockReset();
  listenMock.mockReset();
  resetConverterInstallStateForTest();
});

describe("office-to-PDF conversion", () => {
  it("round-trips bytes through the host command as base64", async () => {
    invokeMock.mockResolvedValue({
      status: "converted",
      pdfBase64: btoa("%PDF-1.7 fake"),
    });

    const pdf = await convertOfficeToPdf(
      new Uint8Array([0x50, 0x4b, 0x03, 0x04]),
      PPTX,
    );

    expect(new TextDecoder().decode(pdf)).toBe("%PDF-1.7 fake");
    expect(invokeMock).toHaveBeenCalledWith("convert_office_to_pdf", {
      request: { contentBase64: btoa("PK\x03\x04"), mediaType: PPTX },
    });
  });

  it("surfaces a missing converter with its install state, not a failure", async () => {
    invokeMock.mockResolvedValue({
      status: "converterMissing",
      installable: true,
      installFailure: "Download cancelled",
    });

    const error = await convertOfficeToPdf(new Uint8Array([1]), PPTX).then(
      () => null,
      (thrown: unknown) => thrown,
    );

    expect(error).toBeInstanceOf(ConverterMissingError);
    const missing = error as ConverterMissingError;
    expect(missing.installable).toBe(true);
    expect(missing.installFailure).toBe("Download cancelled");
  });

  it("preserves complete converter diagnostics for the failure panel", async () => {
    invokeMock.mockResolvedValue({
      status: "failed",
      message: "LibreOffice failed: source file could not be loaded",
      details:
        "Exit status: exit status: 1\n\nStandard error:\nfirst line\nsecond line",
    });

    const error = await convertOfficeToPdf(new Uint8Array([1]), PPTX).then(
      () => null,
      (thrown: unknown) => thrown,
    );

    expect(error).toBeInstanceOf(OfficeConversionError);
    expect(error).toMatchObject({
      message: "LibreOffice failed: source file could not be loaded",
      details:
        "Exit status: exit status: 1\n\nStandard error:\nfirst line\nsecond line",
    });
  });

  it("relays install progress events and tears the listener down after", async () => {
    let emit: ((event: { payload: unknown }) => void) | undefined;
    const unlisten = vi.fn();
    listenMock.mockImplementation(
      (_name: string, handler: (event: { payload: unknown }) => void) => {
        emit = handler;
        return Promise.resolve(unlisten);
      },
    );
    const seen: unknown[] = [];
    invokeMock.mockImplementation(() => {
      // The host emits progress while the install command is in flight.
      emit?.({
        payload: { phase: "downloading", downloadedBytes: 1, totalBytes: 2 },
      });
      return Promise.resolve(undefined);
    });

    await installPresentationConverter((progress) => seen.push(progress));

    expect(invokeMock).toHaveBeenCalledWith("install_presentation_converter");
    expect(seen).toEqual([
      { phase: "downloading", downloadedBytes: 1, totalBytes: 2 },
    ]);
    expect(unlisten).toHaveBeenCalled();
  });

  it("joins an in-flight install: one command, progress and completion to every caller", async () => {
    // The regression this guards: StrictMode disposes the effect run that
    // started the install, and the surviving run must still see progress and
    // resolution rather than a bar frozen at zero.
    let emit: ((event: { payload: unknown }) => void) | undefined;
    listenMock.mockImplementation(
      (_name: string, handler: (event: { payload: unknown }) => void) => {
        emit = handler;
        return Promise.resolve(vi.fn());
      },
    );
    let settle: (() => void) | undefined;
    invokeMock.mockImplementation(
      () => new Promise<void>((resolve) => (settle = resolve)),
    );

    const first: unknown[] = [];
    const second: unknown[] = [];
    const install1 = installPresentationConverter((p) => first.push(p));
    await Promise.resolve();
    emit?.({
      payload: { phase: "downloading", downloadedBytes: 8, totalBytes: 16 },
    });
    // A later mount joins mid-download and is caught up immediately.
    const install2 = installPresentationConverter((p) => second.push(p));
    emit?.({
      payload: { phase: "installing", downloadedBytes: 0, totalBytes: null },
    });
    settle?.();
    await Promise.all([install1, install2]);

    expect(
      invokeMock.mock.calls.filter(
        ([name]) => name === "install_presentation_converter",
      ),
    ).toHaveLength(1);
    expect(first).toEqual([
      { phase: "downloading", downloadedBytes: 8, totalBytes: 16 },
      { phase: "installing", downloadedBytes: 0, totalBytes: null },
    ]);
    expect(second).toEqual([
      { phase: "downloading", downloadedBytes: 8, totalBytes: 16 },
      { phase: "installing", downloadedBytes: 0, totalBytes: null },
    ]);
  });

  it("requests the background warm-up once per app run", () => {
    invokeMock.mockResolvedValue(undefined);
    warmPresentationConverter();
    warmPresentationConverter();
    expect(
      invokeMock.mock.calls.filter(
        ([name]) => name === "warm_presentation_converter",
      ),
    ).toHaveLength(1);
  });

  it("derives the converted source from the original's immutable cache key", async () => {
    invokeMock.mockResolvedValue({
      status: "converted",
      pdfBase64: btoa("%PDF-1.7 converted"),
    });
    const original: FileBytesSource = {
      id: "doc-1",
      cacheKey: "document/chat/doc-1",
      fetch: async () => ({
        bytes: new Uint8Array([0x50, 0x4b]),
        contentType: PPTX,
      }),
    };

    const converted = officePdfSource(original, PPTX);
    expect(converted.cacheKey).toBe("document/chat/doc-1/converted-pdf");

    const fetched = await converted.fetch(new AbortController().signal);
    expect(fetched.contentType).toBe("application/pdf");
    expect(new TextDecoder().decode(fetched.bytes)).toBe("%PDF-1.7 converted");
  });
});
