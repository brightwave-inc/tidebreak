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
 * The footer's one-line revision readout: the current revision and its date,
 * with a total when history has more than one entry. The full list lives in
 * the element's `title`.
 */
function revisionSummary(detail: AppDetail): string {
  const current = detail.revisions.find(
    (revision) => revision.id === detail.current_revision,
  );
  const count =
    detail.revisions.length === 1
      ? null
      : `${detail.revisions.length} revisions`;
  if (!current) return count ?? "1 revision";
  const label = `Revision ${current.ordinal} · ${updatedLabel(current.created_at)}`;
  return count ? `${label} · ${count}` : label;
}

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
      setGrant((current) =>
        current ? { ...current, granted: false } : current,
      );
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

            {grant.granted && actionError && (
              <p className="text-critical mx-4 text-sm" role="alert">
                {actionError}
              </p>
            )}

            <footer className="mx-4 flex flex-wrap items-center gap-x-4 gap-y-2">
              <div
                className="flex min-w-0 flex-1 items-center gap-3"
                aria-label="Access"
              >
                {grant.granted && (
                  <>
                    <div className="text-muted-foreground flex min-w-0 items-center gap-1.5 text-xs">
                      <ShieldCheck
                        className="size-3.5 shrink-0"
                        aria-hidden="true"
                      />
                      <span className="truncate">
                        Allowed to use{" "}
                        {grant.bindings
                          .flatMap((binding) => [
                            ...(binding.operation_ids ?? []),
                            ...(binding.folder !== null
                              ? [
                                  `${binding.name ?? "a folder"} (${
                                    binding.access === "read_write"
                                      ? "read & write"
                                      : "read"
                                  })`,
                                ]
                              : []),
                          ])
                          .join(", ") || "no tools"}
                      </span>
                    </div>
                    <Button
                      variant="outline"
                      size="xs"
                      disabled={busy}
                      onClick={() => void onRevoke()}
                    >
                      Revoke access
                    </Button>
                  </>
                )}
              </div>

              <p
                className="text-muted-foreground text-xs"
                aria-label="Revisions"
                title={detail.revisions
                  .map(
                    (revision) =>
                      `Revision ${revision.ordinal} · ${updatedLabel(
                        revision.created_at,
                      )}${
                        revision.id === detail.current_revision
                          ? " (current)"
                          : ""
                      }`,
                  )
                  .join("\n")}
              >
                {revisionSummary(detail)}{" "}
              </p>

              <Button
                variant="destructive"
                size="xs"
                disabled={busy}
                onClick={() => void onDelete()}
              >
                <Trash2 className="size-3.5" aria-hidden="true" />
                Delete app
              </Button>
            </footer>
          </>
        )}
      </div>
    </div>
  );
}
