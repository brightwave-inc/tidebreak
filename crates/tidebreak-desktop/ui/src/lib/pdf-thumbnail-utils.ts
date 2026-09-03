import type { PdfEngine } from "@embedpdf/models";
import pdfiumWasmUrl from "@embedpdf/pdfium/pdfium.wasm?url";

let sharedEnginePromise: Promise<PdfEngine> | null = null;

export function resolvePdfiumWorkerAssetUrl(
  assetUrl: string,
  pageUrl?: string,
) {
  return pageUrl ? new URL(assetUrl, pageUrl).href : assetUrl;
}

export function loadSharedPdfEngine() {
  sharedEnginePromise ??= import("@embedpdf/engines/pdfium-worker-engine").then(
    ({ createPdfiumEngine }) => {
      // EmbedPDF runs PDFium in a blob-backed worker. A root-relative Vite
      // asset URL cannot be resolved from a blob URL, so hand the worker a
      // fully qualified URL. Keep font fallback local-only as well.
      const workerWasmUrl = resolvePdfiumWorkerAssetUrl(
        pdfiumWasmUrl,
        typeof window === "undefined" ? undefined : window.location.href,
      );

      return createPdfiumEngine(workerWasmUrl, { fontFallback: null });
    },
  );

  return sharedEnginePromise;
}
