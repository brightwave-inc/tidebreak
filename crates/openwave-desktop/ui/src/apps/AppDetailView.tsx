import { ChevronLeft, ExternalLink, ShieldCheck, Trash2, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { toast } from "sonner";

import type { AppDetail, AppGrantState } from "@/api";
import { useConfirm } from "@/components/ConfirmDialog";
import { PanelSecondaryHeader } from "@/components/PanelHeader";
import { Button } from "@/components/ui/button";
import { useManagedPolicy } from "@/managedPolicy";
import { openSignInPage } from "@/openSignInPage";
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
 *
 * A gateway binding adds one affordance the sheet cannot cover: a relayed
 * call the gateway refused for want of the viewer's own credential. Nothing
 * local can supply it, so the banner over the frame just sends the viewer to
 * the gateway to connect the app there.
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
  // The server's message from the last gateway_authorization_required
  // refusal, or null once dismissed. It names the bound app, so the banner
  // does not have to guess which one needs connecting.
  const [connectPrompt, setConnectPrompt] = useState<string | null>(null);
  const generationRef = useRef(0);
  const mutationGenerationRef = useRef(0);
  const refreshGenerationRef = useRef(0);
  const mutationInFlightRef = useRef(false);
  const appIdRef = useRef(appId);
  appIdRef.current = appId;
  const { confirm, dialog } = useConfirm();
  const policy = useManagedPolicy();
  // The gateway page the Publish affordance opens is built from this URL, so
  // an unpaired profile has nowhere to send the author and offers nothing.
  const gatewayPaired = policy.managed && Boolean(policy.gateway_url);

  function scopeIsCurrent(targetAppId: string, scopeGeneration: number) {
    return (
      appIdRef.current === targetAppId &&
      generationRef.current === scopeGeneration
    );
  }

  function mutationIsCurrent(
    targetAppId: string,
    scopeGeneration: number,
    mutationGeneration: number,
  ) {
    return (
      scopeIsCurrent(targetAppId, scopeGeneration) &&
      mutationGenerationRef.current === mutationGeneration
    );
  }

  function refreshIsCurrent(
    targetAppId: string,
    scopeGeneration: number,
    refreshGeneration: number,
  ) {
    return (
      scopeIsCurrent(targetAppId, scopeGeneration) &&
      !mutationInFlightRef.current &&
      refreshGenerationRef.current === refreshGeneration
    );
  }

  useEffect(() => {
    const generation = ++generationRef.current;
    mutationGenerationRef.current += 1;
    refreshGenerationRef.current += 1;
    mutationInFlightRef.current = false;
    setDetail(null);
    setGrant(null);
    setLoadError(null);
    setActionError(null);
    setBusy(false);
    setConnectPrompt(null);
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
    if (mutationInFlightRef.current) return;
    const targetAppId = appId;
    const scopeGeneration = generationRef.current;
    const mutationGeneration = ++mutationGenerationRef.current;
    refreshGenerationRef.current += 1;
    mutationInFlightRef.current = true;
    setBusy(true);
    setActionError(null);
    try {
      const nextGrant = await apis.consent(targetAppId);
      if (
        mutationIsCurrent(targetAppId, scopeGeneration, mutationGeneration)
      ) {
        setGrant(nextGrant);
      }
    } catch (caught) {
      if (
        mutationIsCurrent(targetAppId, scopeGeneration, mutationGeneration)
      ) {
        setActionError(friendlyAppsError(caught, "Could not record consent."));
      }
    } finally {
      if (
        mutationIsCurrent(targetAppId, scopeGeneration, mutationGeneration)
      ) {
        mutationInFlightRef.current = false;
        setBusy(false);
      }
    }
  }

  async function onRevoke() {
    if (mutationInFlightRef.current) return;
    const targetAppId = appId;
    const scopeGeneration = generationRef.current;
    const mutationGeneration = ++mutationGenerationRef.current;
    refreshGenerationRef.current += 1;
    mutationInFlightRef.current = true;
    setBusy(true);
    try {
      await apis.revoke(targetAppId);
      const nextGrant = await apis.grantState(targetAppId);
      if (
        mutationIsCurrent(targetAppId, scopeGeneration, mutationGeneration)
      ) {
        setGrant(nextGrant);
      }
    } catch (caught) {
      if (
        mutationIsCurrent(targetAppId, scopeGeneration, mutationGeneration)
      ) {
        toast.error(friendlyAppsError(caught, "Could not revoke access."));
      }
    } finally {
      if (
        mutationIsCurrent(targetAppId, scopeGeneration, mutationGeneration)
      ) {
        mutationInFlightRef.current = false;
        setBusy(false);
      }
    }
  }

  // A mid-session consent_required refusal (revocation elsewhere, or a
  // server reconfigured underneath the grant) drops the frame back behind
  // the sheet with the server's fresh projection.
  async function onConsentRequired() {
    // A refusal from an operation issued before revoke/delete began is stale
    // relative to that mutation. The mutation's result owns the next state.
    if (mutationInFlightRef.current) return;
    const targetAppId = appId;
    const scopeGeneration = generationRef.current;
    const refreshGeneration = ++refreshGenerationRef.current;
    try {
      const nextGrant = await apis.grantState(targetAppId);
      if (refreshIsCurrent(targetAppId, scopeGeneration, refreshGeneration)) {
        setGrant(nextGrant);
      }
    } catch {
      if (refreshIsCurrent(targetAppId, scopeGeneration, refreshGeneration)) {
        setGrant((current) =>
          current ? { ...current, granted: false } : current,
        );
      }
    }
  }

  // The gateway's own SSO is the handoff: the system browser opens its
  // origin and the viewer's IdP session authenticates there. No credential of
  // ours travels with it — a URL-borne bearer is exactly what the gateway's
  // design rejects.
  async function onConnectAtGateway() {
    try {
      const baseUrl = await apis.gatewayBaseUrl();
      if (!baseUrl) {
        toast.error("This profile is not paired with a model gateway.");
        return;
      }
      await openSignInPage(baseUrl);
      setConnectPrompt(null);
    } catch (caught) {
      toast.error(friendlyAppsError(caught, "Could not open your gateway."));
    }
  }

  // Publishing happens at the gateway, on the app's own page there, next to
  // the publish state and team grants it changes — decision record 11. This
  // host's part is the address, and getting it registers the app there if it
  // never has been, so the page exists by the time the browser arrives.
  async function onPublishAtGateway() {
    const targetAppId = appId;
    setBusy(true);
    try {
      const page = await apis.gatewayPage(targetAppId);
      if (page.outcome === "ready" && page.url) {
        await openSignInPage(page.url);
        return;
      }
      // The gateway's own words wherever it had any: a bundle it will not
      // hold names what about it, and nothing assembled here could.
      toast.error(
        page.message ??
          (page.outcome === "no_gateway"
            ? "This profile is not paired with a model gateway."
            : "Your gateway does not hold shared apps, so this app has no page there."),
      );
    } catch (caught) {
      toast.error(friendlyAppsError(caught, "Could not open your gateway."));
    } finally {
      if (appIdRef.current === targetAppId) setBusy(false);
    }
  }

  async function onDelete() {
    const targetAppId = appId;
    const scopeGeneration = generationRef.current;
    const confirmed = await confirm({
      title: `Delete ${detail?.name ?? "this app"}?`,
      description:
        "The app will be removed from the library and can no longer be opened.",
      confirmLabel: "Delete app",
      destructive: true,
    });
    if (
      !confirmed ||
      appIdRef.current !== targetAppId ||
      generationRef.current !== scopeGeneration
    ) {
      return;
    }
    if (mutationInFlightRef.current) return;
    const mutationGeneration = ++mutationGenerationRef.current;
    refreshGenerationRef.current += 1;
    mutationInFlightRef.current = true;
    setBusy(true);
    try {
      await apis.deleteApp(targetAppId);
      if (
        mutationIsCurrent(targetAppId, scopeGeneration, mutationGeneration)
      ) {
        onBack();
      }
    } catch (caught) {
      if (
        mutationIsCurrent(targetAppId, scopeGeneration, mutationGeneration)
      ) {
        toast.error(friendlyAppsError(caught, "Could not delete this app."));
      }
    } finally {
      if (
        mutationIsCurrent(targetAppId, scopeGeneration, mutationGeneration)
      ) {
        mutationInFlightRef.current = false;
        setBusy(false);
      }
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
              <>
                {connectPrompt && (
                  <div
                    className="border-warning/40 bg-warning/10 mx-4 flex items-start gap-3 rounded-lg border p-3"
                    role="alert"
                  >
                    <p className="min-w-0 flex-1 text-sm">{connectPrompt}</p>
                    <Button
                      variant="outline"
                      size="xs"
                      onClick={() => void onConnectAtGateway()}
                    >
                      <ExternalLink className="size-3.5" aria-hidden="true" />
                      Connect at gateway
                    </Button>
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      onClick={() => setConnectPrompt(null)}
                    >
                      <X className="size-4" />
                      <span className="sr-only">Dismiss</span>
                    </Button>
                  </div>
                )}
                <AppFrame
                  appId={appId}
                  name={detail.name}
                  apis={apis}
                  onConsentRequired={() => void onConsentRequired()}
                  onGatewayConnectRequired={setConnectPrompt}
                />
              </>
            ) : (
              <AppConsentSheet
                state={grant}
                busy={busy}
                error={actionError}
                onConsent={() => void onConsent()}
              />
            )}

            <footer className="mx-4 flex flex-wrap items-center gap-x-4 gap-y-2">
              <div
                className="flex min-w-0 flex-1 items-center gap-3"
                aria-label="Access"
              >
                {/* A bindingless app is granted vacuously and holds nothing
                    revocable, so the access readout and revoke control stay
                    hidden. */}
                {grant.granted && grant.bindings.length > 0 && (
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

              {/* Offered only for an app that actually uses the gateway --
                  the same bindings registration follows -- and only on a
                  profile paired with one, since the page it opens is built
                  from the managed policy's gateway URL. */}
              {gatewayPaired &&
                grant.bindings.some(
                  (binding) => binding.gateway_app !== null,
                ) && (
                  <Button
                    variant="outline"
                    size="xs"
                    disabled={busy}
                    onClick={() => void onPublishAtGateway()}
                  >
                    <ExternalLink className="size-3.5" aria-hidden="true" />
                    Publish at gateway
                  </Button>
                )}

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
      {dialog}
    </div>
  );
}
