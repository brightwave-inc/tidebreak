import type { IWorkbookData } from "@univerjs/presets";
import { useCallback, useEffect, useRef, useState } from "react";

import UniverParserWorker from "@/workers/univer-parser.worker?worker&inline";

// Singleton worker instance (persists across component unmounts to preserve cache)
let globalWorker: Worker | null = null;

function getWorkerInstance(): Worker {
  if (!globalWorker) {
    globalWorker = new UniverParserWorker();
  }
  return globalWorker;
}

export interface WorkerParseResult {
  workbookData: IWorkbookData;
}

/**
 * Parse a workbook off the main thread.
 *
 * A spreadsheet of any size takes long enough to read that doing it inline
 * freezes the window; the worker keeps the app responsive and caches parsed
 * workbooks so reopening a source is instant.
 */
export function useUniverWorker() {
  const workerRef = useRef<Worker | null>(null);
  const [isProcessing, setIsProcessing] = useState(false);

  const pendingOpsRef = useRef(
    new Map<
      string,
      {
        resolve: (result: WorkerParseResult) => void;
        reject: (error: Error) => void;
      }
    >(),
  );

  useEffect(() => {
    workerRef.current = getWorkerInstance();
    const worker = workerRef.current;
    const pendingOps = pendingOpsRef.current;

    const handleMessage = (e: MessageEvent) => {
      const message = e.data;
      const opId = message.opId;

      switch (message.type) {
        case "cache-miss": {
          if (!pendingOps.has(opId)) break;
          setIsProcessing(true);
          break;
        }

        case "result": {
          const pending = pendingOps.get(opId);
          if (pending) {
            pending.resolve({ workbookData: message.workbookData });
            pendingOps.delete(opId);
            if (pendingOps.size === 0) setIsProcessing(false);
          }
          break;
        }

        case "error": {
          const pending = pendingOps.get(opId);
          if (pending) {
            pending.reject(new Error(message.error));
            pendingOps.delete(opId);
            if (pendingOps.size === 0) setIsProcessing(false);
          }
          break;
        }
      }
    };

    const handleError = (event: ErrorEvent) => {
      const err = new Error(event.message || "the spreadsheet parser stopped");
      pendingOps.forEach((pending) => pending.reject(err));
      pendingOps.clear();
      setIsProcessing(false);
    };

    worker.addEventListener("message", handleMessage);
    worker.addEventListener("error", handleError);

    return () => {
      worker.removeEventListener("message", handleMessage);
      worker.removeEventListener("error", handleError);
    };
  }, []);

  const parseWorkbook = useCallback(
    (
      data: ArrayBuffer,
      documentId: string,
      options?: { isCsv?: boolean },
    ): Promise<WorkerParseResult> => {
      return new Promise((resolve, reject) => {
        if (!workerRef.current) {
          reject(new Error("the spreadsheet parser is not running"));
          return;
        }

        const opId = `univer:${documentId}:${Date.now()}`;
        pendingOpsRef.current.set(opId, { resolve, reject });

        const clonedData = data.slice(0);
        workerRef.current.postMessage(
          {
            type: "parse",
            data: clonedData,
            documentId,
            opId,
            isCsv: options?.isCsv,
          },
          [clonedData],
        );
      });
    },
    [],
  );

  const clearCache = useCallback((documentId: string) => {
    workerRef.current?.postMessage({ type: "clear-cache", documentId });
  }, []);

  return { parseWorkbook, clearCache, isProcessing };
}
