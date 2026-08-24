import { Loader2, TriangleAlert } from "lucide-react";

import type { CodeHarnessInstallSnapshot } from "../api/types";
import { HARNESS_LABELS } from "./labels";

/**
 * What a harness download is doing, under the engine picker.
 *
 * A pinned engine is a 37-297MB npm install the first time its version is
 * used. Create used to pay for it with nothing on screen, so the surface that
 * knows which engine is next starts the download and says so here. Engines
 * already on disk render nothing: the picker already shows a usable engine.
 */
export function HarnessInstallNote({
  install,
}: {
  install: CodeHarnessInstallSnapshot | undefined;
}) {
  if (!install || (install.done && !install.error)) return null;
  const label = HARNESS_LABELS[install.kind];
  const version = install.version ? ` ${install.version}` : "";
  if (install.error) {
    return (
      <p className="text-destructive flex items-start gap-1.5 text-xs">
        <TriangleAlert
          className="mt-0.5 size-3.5 shrink-0"
          aria-hidden="true"
        />
        <span>
          {label}
          {version} could not be downloaded. {install.error}
        </span>
      </p>
    );
  }
  return (
    <p
      className="text-muted-foreground flex items-center gap-1.5 text-xs"
      role="status"
    >
      <Loader2 className="size-3.5 shrink-0 animate-spin" aria-hidden="true" />
      <span>
        Downloading {label}
        {version}. This runs once, and takes a few minutes.
      </span>
    </p>
  );
}
