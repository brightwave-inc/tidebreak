import { isTauri } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";

import {
  observeConverterInstallProgress,
  type ConverterInstallProgress,
} from "@/document/officePdf";

/**
 * How long a silent host counts as "finished" and clears the line.
 *
 * The host reports progress but never completion, so idleness is the only
 * end-of-install signal available here. Generous on purpose: this is a quiet
 * status line, and blinking it off between two slow chunks would read worse
 * than leaving it up a few seconds past the end.
 */
const IDLE_MS = 5_000;

/**
 * The host tool being provisioned right now, for a panel that wants to say so
 * quietly.
 *
 * Nothing here starts an install: enabling a plugin does that host-side, and
 * this only listens to the progress that pass emits. `null` whenever no
 * install is under way — including everywhere the host emits nothing at all,
 * which is every platform but macOS today.
 */
export function useHostToolProvisioning(): ConverterInstallProgress | null {
  const [progress, setProgress] = useState<ConverterInstallProgress | null>(null);

  useEffect(() => {
    if (!isTauri()) return;
    let disposed = false;
    let idle: ReturnType<typeof setTimeout> | undefined;
    let unlisten: (() => void) | undefined;
    void observeConverterInstallProgress((next) => {
      if (disposed) return;
      setProgress(next);
      clearTimeout(idle);
      idle = setTimeout(() => setProgress(null), IDLE_MS);
    })
      .then((dispose) => {
        if (disposed) dispose();
        else unlisten = dispose;
      })
      .catch(() => {
        // Best-effort: without the host bridge there is simply nothing to show.
      });
    return () => {
      disposed = true;
      clearTimeout(idle);
      unlisten?.();
    };
  }, []);

  return progress;
}

/** The one muted line the panel renders for `progress`. */
export function hostToolProvisioningLabel(
  progress: ConverterInstallProgress,
): string {
  if (progress.phase === "installing") return "Preparing document tools…";
  if (progress.totalBytes === null || progress.totalBytes <= 0) {
    return "Preparing document tools…";
  }
  const percent = Math.min(
    100,
    Math.round((progress.downloadedBytes / progress.totalBytes) * 100),
  );
  return `Preparing document tools… ${percent}%`;
}
