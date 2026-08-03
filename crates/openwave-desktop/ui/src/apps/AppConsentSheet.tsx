import { ShieldAlert, ShieldCheck, TriangleAlert, Wrench } from "lucide-react";

import type { AppGrantState } from "@/api";
import { Button } from "@/components/ui/button";

/**
 * The consent sheet: exactly what the server's grant projection says, and a
 * single affirmative.
 *
 * The sheet never composes what is being granted — the server recomputes the
 * grant from the app's current manifest and the definitions current at that
 * moment, so consenting here is only ever "yes to what is shown". A server
 * whose definition changed since a previous consent carries a visible marker:
 * consent named a definition, not a name, and must not silently survive it.
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
        <ul className="flex flex-col gap-2" aria-label="Requested tool access">
          {state.bindings.map((binding) => (
            <li key={binding.app} className="flex flex-col gap-1">
              <div className="flex items-center gap-2 text-sm">
                <span className="font-medium">
                  {binding.name ?? "Unknown connected app"}
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
                    Reconfigured since you agreed
                  </span>
                )}
              </div>
              <ul className="flex flex-col gap-0.5">
                {binding.tools.map((tool) => (
                  <li
                    key={tool}
                    className="text-muted-foreground flex items-center gap-1.5 pl-1 text-xs"
                  >
                    <Wrench className="size-3 shrink-0" aria-hidden="true" />
                    <span className="truncate font-mono">{tool}</span>
                  </li>
                ))}
              </ul>
            </li>
          ))}
        </ul>
      )}
      <p className="text-muted-foreground text-xs">
        The app can call only these tools, only while you have it open. You can
        revoke this at any time from the app&rsquo;s page.
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
