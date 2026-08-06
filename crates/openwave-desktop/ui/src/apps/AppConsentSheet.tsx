import {
  FolderOpen,
  Globe,
  PencilLine,
  ShieldAlert,
  ShieldCheck,
  TriangleAlert,
} from "lucide-react";

import type { AppGrantState } from "@/api";
import { Button } from "@/components/ui/button";

/**
 * The consent sheet: exactly what the server's grant projection says, and a
 * single affirmative.
 *
 * The sheet never composes what is being granted — the server recomputes the
 * grant from the app's current manifest and the definitions and folder
 * registrations current at that moment, so consenting here is only ever "yes
 * to what is shown". A target that changed since a previous consent carries a
 * visible marker: consent named a definition, not a name, and must not
 * silently survive it.
 *
 * A manifest that binds both a folder and API operations gets the explicit
 * combined-consent warning (docs/folder-bindings.md): the app can read files
 * and send data out, and the sheet says so before the user agrees.
 */
export function AppConsentSheet({
  state,
  busy,
  error,
  onConsent,
}: {
  state: AppGrantState;
  busy: boolean;
  error: string | null;
  onConsent: () => void;
}) {
  const folderNames = state.bindings
    .filter((binding) => binding.folder !== null)
    .map((binding) => binding.name ?? "an unknown folder");
  const networkNames = state.bindings
    .filter((binding) => binding.operation_ids !== null)
    .map((binding) => binding.name ?? "an unknown connected app");
  const exfiltrationWarning =
    folderNames.length > 0 && networkNames.length > 0
      ? `This app can read ${formatNames(folderNames)} and send data to ${formatNames(networkNames)}.`
      : null;
  return (
    <section
      className="bg-background mx-4 flex flex-col gap-3 rounded-lg border p-4"
      aria-label="App access consent"
    >
      <div className="flex items-center gap-2">
        <ShieldAlert className="text-warning size-4 shrink-0" aria-hidden="true" />
        <h2 className="text-sm font-medium">This app needs your permission</h2>
      </div>
      {state.bindings.length === 0 ? (
        <p className="text-muted-foreground text-sm">
          This app requests no tool access.
        </p>
      ) : (
        <ul className="flex flex-col gap-2" aria-label="Requested access">
          {state.bindings.map((binding) => (
            <li
              key={binding.app ?? binding.folder ?? binding.name ?? ""}
              className="flex flex-col gap-1"
            >
              <div className="flex items-center gap-2 text-sm">
                {binding.folder !== null && (
                  <FolderOpen
                    className="text-muted-foreground size-3.5 shrink-0"
                    aria-hidden="true"
                  />
                )}
                <span className="font-medium">
                  {binding.name ??
                    (binding.folder !== null
                      ? "Unknown folder"
                      : "Unknown connected app")}
                </span>
                {binding.granted && (
                  <span className="text-muted-foreground flex items-center gap-1 text-xs">
                    <ShieldCheck className="size-3" aria-hidden="true" />
                    granted
                  </span>
                )}
                {binding.definition_changed && (
                  <span className="text-warning flex items-center gap-1 text-xs">
                    <TriangleAlert className="size-3" aria-hidden="true" />
                    {binding.folder !== null
                      ? "Changed since you agreed"
                      : "Reconfigured since you agreed"}
                  </span>
                )}
              </div>
              {binding.access !== null && (
                <div className="flex flex-col gap-0.5">
                  <span className="text-muted-foreground flex items-center gap-1.5 pl-1 text-xs">
                    <FolderOpen className="size-3 shrink-0" aria-hidden="true" />
                    Read files and folders
                  </span>
                  {binding.access === "read_write" && (
                    <span className="text-warning flex items-center gap-1.5 pl-1 text-xs">
                      <PencilLine className="size-3 shrink-0" aria-hidden="true" />
                      Create and replace files in this folder
                    </span>
                  )}
                </div>
              )}
              <ul className="flex flex-col gap-0.5">
                {(binding.operation_ids ?? []).map((operationId) => (
                  <li
                    key={operationId}
                    className="text-muted-foreground flex items-center gap-1.5 pl-1 text-xs"
                  >
                    <Globe className="size-3 shrink-0" aria-hidden="true" />
                    <span className="truncate font-mono">{operationId}</span>
                  </li>
                ))}
              </ul>
            </li>
          ))}
        </ul>
      )}
      {exfiltrationWarning && (
        <p
          className="border-warning/50 text-warning flex items-start gap-2 rounded-md border px-3 py-2 text-xs"
          role="alert"
        >
          <TriangleAlert className="mt-0.5 size-3.5 shrink-0" aria-hidden="true" />
          <span>{exfiltrationWarning}</span>
        </p>
      )}
      <p className="text-muted-foreground text-xs">
        The app can use only this access, only while you have it open. You can
        revoke it at any time from the app&rsquo;s page.
      </p>
      {error && (
        <p className="text-critical text-sm" role="alert">
          {error}
        </p>
      )}
      <div>
        <Button size="sm" disabled={busy} onClick={onConsent}>
          Allow access
        </Button>
      </div>
    </section>
  );
}

/** "A", "A and B", or "A, B, and C" — the warning names every side in prose. */
function formatNames(names: string[]): string {
  const quoted = names.map((name) => `'${name}'`);
  if (quoted.length === 1) return quoted[0];
  if (quoted.length === 2) return `${quoted[0]} and ${quoted[1]}`;
  return `${quoted.slice(0, -1).join(", ")}, and ${quoted[quoted.length - 1]}`;
}
