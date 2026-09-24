import { Spinner } from "@/components/ui/spinner";

import type { CodeHarnessInstallSnapshot } from "../api/types";
import { HARNESS_LABELS } from "./labels";
import { Notice } from "@/components/ui/notice";

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
      <Notice tone="critical" density="compact">
        {label}
        {version} could not be downloaded. {install.error}
      </Notice>
    );
  }
  return (
    <p
      className="text-muted-foreground flex items-center gap-1.5 text-xs"
      role="status"
    >
      <Spinner className="size-3.5" aria-hidden="true" />
      <span>
        Downloading {label}
        {version}. This runs once, and takes a few minutes.
      </span>
    </p>
  );
}
