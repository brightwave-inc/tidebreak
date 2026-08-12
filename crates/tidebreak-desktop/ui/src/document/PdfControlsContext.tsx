import type { PropsWithChildren } from "react";
import { createContext, use, useState } from "react";

/** Page state the PDF viewer publishes for an out-of-tree consumer. */
export interface PdfControlsData {
  currentPage: number;
  numPages: number;
  setPage: (page: number) => void;
}

interface PdfControlsContextValue {
  controls: PdfControlsData | null;
  setControls: (controls: PdfControlsData | null) => void;
}

const PdfControlsContext = createContext<PdfControlsContextValue | null>(null);

/**
 * Bridges the PDF viewer's page picker up to the panel header so the page and
 * zoom controls can live in the header row instead of floating over the
 * document. Zoom is already an app-wide store (useZoom); only the per-viewer
 * page state needs bridging. Optional — the viewer falls back to its own
 * toolbar when no provider is present.
 */
export function PdfControlsProvider({ children }: PropsWithChildren) {
  const [controls, setControls] = useState<PdfControlsData | null>(null);
  return (
    <PdfControlsContext value={{ controls, setControls }}>
      {children}
    </PdfControlsContext>
  );
}

/** Header-side: read the registered page controls (null when none). */
export function usePdfControls(): PdfControlsData | null {
  return use(PdfControlsContext)?.controls ?? null;
}

/**
 * Viewer-side: the registration callback, or null when there is no provider
 * (e.g. the PDF viewer rendered outside a panel).
 */
export function useRegisterPdfControls():
  | ((controls: PdfControlsData | null) => void)
  | null {
  return use(PdfControlsContext)?.setControls ?? null;
}
