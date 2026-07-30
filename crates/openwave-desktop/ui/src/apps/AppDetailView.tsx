import { ChevronLeft, ShieldCheck, Trash2 } from "lucide-react";
import { useEffect, useRef, useState } from "react";

import type { AppDetail, AppGrantState } from "@/api";
import { PanelSecondaryHeader } from "@/components/PanelHeader";
import { Button } from "@/components/ui/button";
import { AppConsentSheet } from "./AppConsentSheet";
import { AppFrame } from "./AppFrame";
import { friendlyAppsError, updatedLabel } from "./AppsView";
import type { AppsApis } from "./appsApis";

/**
 * One app, as the panel addressed `apps.{appId}`: the running frame (behind
 * its consent gate), the grant controls, the revision history, and deletion.
 *
 * The open flow is a consent check first, always: the server's grant verdict
 * decides whether the frame mounts or the sheet renders, and the frame's own
 * `consent_required` refusals fold back into the same gate — the sheet is the
 * one way through, and the server recomputes what it grants.
 */
export function AppDetailView({
  appId,
  apis,
  onBack,
}: {
  appId: string;
  apis: AppsApis;
  /** Return to the `apps` list panel; also where deletion lands. */
  onBack: () => void;
}) {
  const [detail, setDetail] = useState<AppDetail | null>(null);
  const [grant, setGrant] = useState<AppGrantState | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const generationRef = useRef(0);

  useEffect(() => {
    const generation = ++generationRef.current;
    setDetail(null);
    setGrant(null);
    setLoadError(null);
    setActionError(null);
    void (async () => {
      try {
        const [loadedDetail, loadedGrant] = await Promise.all([
          apis.get(appId),
          apis.grantState(appId),
        ]);
        if (generation !== generationRef.current) return;
        setDetail(loadedDetail);
        setGrant(loadedGrant);
      } catch (caught) {
        if (generation !== generationRef.current) return;
        setLoadError(friendlyAppsError(caught, "Could not load this app."));
      }
    })();
    return () => {
      generationRef.current += 1;
    };
  }, [apis, appId]);

  async function onConsent() {
    setBusy(true);
    setActionError(null);
    try {
      setGrant(await apis.consent(appId));
    } catch (caught) {
      setActionError(friendlyAppsError(caught, "Could not record consent."));
    } finally {
      setBusy(false);
    }
  }

  async function onRevoke() {
    setBusy(true);
    setActionError(null);
    try {
      await apis.revoke(appId);
      setGrant(await apis.grantState(appId));
    } catch (caught) {
      setActionError(friendlyAppsError(caught, "Could not revoke access."));
    } finally {
      setBusy(false);
    }
  }

  // A mid-session consent_required refusal (revocation elsewhere, or a
  // server reconfigured underneath the grant) drops the frame back behind
  // the sheet with the server's fresh projection.
  async function onConsentRequired() {
    try {
      setGrant(await apis.grantState(appId));
    } catch {
      setGrant((current) => (current ? { ...current, granted: false } : current));
    }
  }

  async function onDelete() {
    setBusy(true);
    setActionError(null);
    try {
      await apis.deleteApp(appId);
      onBack();
    } catch (caught) {
      setActionError(friendlyAppsError(caught, "Could not delete this app."));
      setBusy(false);
    }
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <PanelSecondaryHeader showBorder={false} className="pr-1 pl-2">
        <Button variant="ghost" size="icon-sm" onClick={onBack}>
          <ChevronLeft className="size-4" />
          <span className="sr-only">Back to apps</span>
        </Button>
        <h1 className="min-w-0 truncate text-lg font-medium">
          {detail?.name ?? "App"}
        </h1>
      </PanelSecondaryHeader>

      <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto pt-4 pb-4">
        {loadError && (
          <p className="text-critical mx-4 text-sm" role="alert">
            {loadError}
          </p>
        )}
        {!loadError && (!detail || !grant) && (
          <p className="text-muted-foreground mx-4 text-sm" role="status">
            Loading app…
          </p>
        )}

        {detail && grant && (
          <>
            {grant.granted ? (
              <AppFrame
                appId={appId}
                name={detail.name}
                apis={apis}
                onConsentRequired={() => void onConsentRequired()}
              />
            ) : (
              <AppConsentSheet
                state={grant}
                busy={busy}
                error={actionError}
                onConsent={() => void onConsent()}
              />
            )}

            {grant.granted && (
              <section className="mx-4 flex flex-col gap-2" aria-label="Access">
                <div className="text-muted-foreground flex items-center gap-1.5 text-xs">
                  <ShieldCheck className="size-3.5 shrink-0" aria-hidden="true" />
                  <span>
                    Allowed to call{" "}
                    {grant.bindings
                      .flatMap((binding) => binding.tools)
                      .join(", ") || "no tools"}
                  </span>
                </div>
                {actionError && (
                  <p className="text-critical text-sm" role="alert">
                    {actionError}
                  </p>
                )}
                <div>
                  <Button
                    variant="outline"
                    size="xs"
                    disabled={busy}
                    onClick={() => void onRevoke()}
                  >
                    Revoke access
                  </Button>
                </div>
              </section>
            )}

            <section className="mx-4 flex flex-col gap-1" aria-label="Revisions">
              <h2 className="text-sm font-medium">
                {detail.revisions.length === 1
                  ? "1 revision"
                  : `${detail.revisions.length} revisions`}
              </h2>
              <ul className="flex flex-col gap-0.5">
                {detail.revisions.map((revision) => (
                  <li
                    key={revision.id}
                    className="text-muted-foreground flex items-center gap-2 text-xs"
                  >
                    <span className="tabular-nums">
                      Revision {revision.ordinal}
                    </span>
                    <span>{updatedLabel(revision.created_at)}</span>
                    {revision.id === detail.current_revision && (
                      <span className="text-foreground">current</span>
                    )}
                  </li>
                ))}
              </ul>
            </section>

            <section className="mx-4" aria-label="Delete app">
              <Button
                variant="outline"
                size="xs"
                disabled={busy}
                onClick={() => void onDelete()}
              >
                <Trash2 className="size-3.5" aria-hidden="true" />
                Delete app
              </Button>
            </section>
          </>
        )}
      </div>
    </div>
  );
}
