import type {
  ConversationExportRequest,
  DataOverview,
  RuntimeSettings,
} from "../types";
import { type Constructor, HttpCore, throwIfNotOk } from "./http";

/** A file the server answered with, for a browser that saves it itself. */
export type DownloadedFile = {
  blob: Blob;
  fileName: string;
  /** Files in a backup, or conversations in an export, when the server said. */
  count: number | null;
};

/**
 * The `/data` route family: where the profile lives, a backup of it, and
 * exports of the caller's conversations, plus the settings reset that sits
 * beside them on the Data and privacy page.
 *
 * On the desktop the backup and the export go through the native save dialog
 * instead (see `host.ts`), which streams straight to disk. These downloads
 * are for a browser, which has no dialog to hand a path to.
 */
export function withDataApi<TBase extends Constructor<HttpCore>>(Base: TBase) {
  return class extends Base {
    /** Where the profile lives, which database holds it, and its disk use. */
    getDataOverview(): Promise<DataOverview> {
      return this.json("/data", { headers: this.headers() });
    }

    /** Put the settings pages' preferences back to their defaults. */
    resetSettings(): Promise<RuntimeSettings> {
      return this.json("/settings/reset", {
        method: "POST",
        headers: this.headers(),
      });
    }

    downloadProfileBackup(): Promise<DownloadedFile> {
      return this.downloadDataFile(
        "/data/backup",
        { method: "POST", headers: this.headers() },
        "Tidebreak backup.tar.gz",
        "x-tidebreak-backup-files",
      );
    }

    downloadConversationExport(
      request: ConversationExportRequest,
    ): Promise<DownloadedFile> {
      return this.downloadDataFile(
        "/data/export",
        {
          method: "POST",
          headers: this.headers(true),
          body: JSON.stringify(request),
        },
        request.format === "json"
          ? "Tidebreak conversations.json"
          : "Tidebreak conversations.zip",
        "x-tidebreak-conversations",
      );
    }

    protected async downloadDataFile(
      path: string,
      init: RequestInit,
      fallbackName: string,
      countHeader: string,
    ): Promise<DownloadedFile> {
      const response = await fetch(`${this.baseUrl}${path}`, init);
      await throwIfNotOk(response);
      const reported = response.headers.get(countHeader);
      const count = reported === null ? Number.NaN : Number(reported);
      return {
        blob: await response.blob(),
        fileName:
          dispositionFileName(response.headers.get("Content-Disposition")) ??
          fallbackName,
        count: Number.isSafeInteger(count) ? count : null,
      };
    }
  };
}

/** The `filename` a `Content-Disposition: attachment` header names. */
export function dispositionFileName(header: string | null): string | null {
  const match = header?.match(/filename="([^"]+)"/);
  return match ? match[1] : null;
}
