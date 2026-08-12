import { useCallback, useEffect, useRef, useState } from "react";

const STORAGE_KEY_PREFIX = "pdf_page_";

function getStoredPage(documentId: string): number | null {
  const stored = window.sessionStorage.getItem(`${STORAGE_KEY_PREFIX}${documentId}`);
  if (!stored) return null;
  const page = parseInt(stored, 10);
  return Number.isFinite(page) ? page : null;
}

function setStoredPage(documentId: string, page: number): void {
  window.sessionStorage.setItem(`${STORAGE_KEY_PREFIX}${documentId}`, String(page));
}

function clampPage(page: number, numPages: number) {
  const upper = numPages > 0 ? numPages : Number.POSITIVE_INFINITY;
  return Math.min(Math.max(1, page), upper);
}

interface UsePdfPageStateOptions {
  numPages: number;
  targetPage?: number;
}

interface UsePdfPageStateReturn {
  currentPage: number;
  setCurrentPage: (page: number | ((prev: number) => number)) => void;
}

/**
 * Which page of a document you were on, remembered per document for as long as
 * the app is running. Closing the panel and opening the same source again puts
 * you back where you were instead of at page one, which is what makes a long
 * PDF usable at all. Session storage rather than a store because the memory
 * should not outlive the app session.
 */
export function usePdfPageState(
  documentId: string,
  { numPages, targetPage }: UsePdfPageStateOptions,
): UsePdfPageStateReturn {
  const [currentPage, setCurrentPageInternal] = useState(
    () => getStoredPage(documentId) ?? 1,
  );

  // Reset when document changes
  useEffect(() => {
    setCurrentPageInternal(getStoredPage(documentId) ?? 1);
  }, [documentId]);

  // Clamp when numPages becomes known; keep storage in sync
  useEffect(() => {
    if (numPages <= 0) return;
    setCurrentPageInternal((prev) => {
      const clamped = clampPage(prev, numPages);
      if (clamped !== prev) setStoredPage(documentId, clamped);
      return clamped;
    });
  }, [numPages, documentId]);

  // Apply targetPage once per (documentId, targetPage)
  const lastAppliedTargetRef = useRef<string>("");
  useEffect(() => {
    if (targetPage == null || numPages <= 0) return;
    const key = `${documentId}:${targetPage}`;
    if (lastAppliedTargetRef.current === key) return;

    const clamped = clampPage(targetPage, numPages);
    lastAppliedTargetRef.current = key;

    setCurrentPageInternal(clamped);
    setStoredPage(documentId, clamped);
  }, [targetPage, numPages, documentId]);

  const setCurrentPage = useCallback(
    (pageOrUpdater: number | ((prev: number) => number)) => {
      setCurrentPageInternal((prev) => {
        const raw =
          typeof pageOrUpdater === "function" ? pageOrUpdater(prev) : pageOrUpdater;

        const next = clampPage(raw, numPages);
        setStoredPage(documentId, next);
        return next;
      });
    },
    [documentId, numPages],
  );

  return { currentPage, setCurrentPage };
}
