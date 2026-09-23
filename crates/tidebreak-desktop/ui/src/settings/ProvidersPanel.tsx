import { useCallback, useEffect, useRef, useState } from "react";
import type { RefObject } from "react";
import { ChevronRight } from "lucide-react";
import { toast } from "sonner";
import type { ApiClient, ModelInfo, ProviderInfo, ProviderKind } from "../api";
import { openInBrowser } from "../openInBrowser";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { cn } from "@/lib/utils";
import { hostMachineLabel } from "@/remoteMachine";
import { useConfirm } from "../components/ConfirmDialog";
import { SettingsError, SettingsPanel, SettingsSection } from "./primitives";
import { ProviderIcon } from "../ProviderIcons";
import { providerLabel } from "../ModelSelection";
import { ProviderModelsSection } from "./ProviderModelsSection";

const CHATGPT_SIGN_IN_POLL_MS = 2_000;
// Matches the server's sign-in window; polling past it can only report a
// timeout the server has already recorded.
const CHATGPT_SIGN_IN_TIMEOUT_MS = 5 * 60 * 1000;

const EXPANDED_PROVIDERS_KEY = "tidebreak.settings.providers-expanded";

/**
 * Which cards were left open, remembered across visits. Best-effort like the
 * rail's own collapse preference: a browser that refuses storage just opens
 * every card collapsed again.
 */
function readExpandedProviders(): Record<string, boolean> {
  try {
    const raw = window.localStorage.getItem(EXPANDED_PROVIDERS_KEY);
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object") return {};
    return Object.fromEntries(
      Object.entries(parsed as Record<string, unknown>).map(([kind, open]) => [
        kind,
        open === true,
      ]),
    );
  } catch {
    return {};
  }
}

function storeExpandedProviders(state: Record<string, boolean>): void {
  try {
    window.localStorage.setItem(EXPANDED_PROVIDERS_KEY, JSON.stringify(state));
  } catch {
    // Preference persistence is best-effort.
  }
}

export function ProvidersPanel({
  providers,
  models = [],
  client,
  managed = false,
  onChanged,
  expandProvider,
  focusCredential = false,
}: {
  providers: ProviderInfo[];
  /** The catalog, so each card can list the models it contributes to pickers. */
  models?: ModelInfo[];
  client: ApiClient;
  /** A managed profile's models all come from its gateway, and the server
   * refuses every credential and endpoint write here — so the editors are not
   * shown at all rather than offered and then rejected. */
  managed?: boolean;
  onChanged: () => void;
  /** Deep link: the provider whose card opens and scrolls into view. */
  expandProvider?: string;
  /** Deep link: also put the cursor in that card's credential field. */
  focusCredential?: boolean;
}) {
  const [expanded, setExpanded] = useState<Record<string, boolean>>(
    readExpandedProviders,
  );
  const setExpandedFor = useCallback((kind: string, open: boolean) => {
    setExpanded((current) => {
      const next = { ...current, [kind]: open };
      storeExpandedProviders(next);
      return next;
    });
  }, []);

  // A deep link opens its card without disturbing the others, and the choice
  // sticks the way a click would.
  useEffect(() => {
    if (expandProvider) setExpandedFor(expandProvider, true);
  }, [expandProvider, setExpandedFor]);

  if (managed) {
    return (
      <SettingsPanel
        title="Providers"
        description="This Tidebreak is managed by your organization."
      >
        <SettingsSection>
          <p className="text-sm text-muted-foreground">
            Model providers are configured by your organization&apos;s model
            gateway. Your own API keys and endpoints are not used, and cannot be
            added here. See the Model Gateway section for the models and tools
            available to you.
          </p>
        </SettingsSection>
      </SettingsPanel>
    );
  }
  return (
    <SettingsPanel
      title="Providers"
      description={`Credentials stay on ${hostMachineLabel()}. Enable a provider, then save a credential.`}
    >
      <div className="flex flex-col gap-2">
        {providers
          // The gateway signs in with OAuth, not a pasted key; its whole
          // surface (connect, identity, entitled models) lives in the
          // dedicated Model Gateway settings panel.
          .filter((p) => p.kind !== "model_gateway")
          .map((p) => (
            <ProviderRow
              key={p.kind}
              info={p}
              client={client}
              onChanged={onChanged}
              catalogModels={models.filter(
                (model) => model.provider === p.kind,
              )}
              expanded={expanded[p.kind] === true}
              onExpandedChange={(open) => setExpandedFor(p.kind, open)}
              deepLinked={expandProvider === p.kind}
              deepLinkFocusesCredential={
                expandProvider === p.kind && focusCredential
              }
            />
          ))}
      </div>
    </SettingsPanel>
  );
}

function ProviderRow({
  info,
  client,
  onChanged,
  catalogModels,
  expanded,
  onExpandedChange,
  deepLinked,
  deepLinkFocusesCredential,
}: {
  info: ProviderInfo;
  client: ApiClient;
  onChanged: () => void;
  catalogModels: ModelInfo[];
  expanded: boolean;
  onExpandedChange: (open: boolean) => void;
  deepLinked: boolean;
  deepLinkFocusesCredential: boolean;
}) {
  const [key, setKey] = useState("");
  const [baseUrl, setBaseUrl] = useState(info.base_url ?? "");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const { confirm, dialog } = useConfirm();
  const acceptsBaseUrl =
    info.kind === "openai_compatible" || info.kind === "ollama";
  const requiresCredential = info.kind !== "ollama";
  const [pendingCredentialFocus, setPendingCredentialFocus] = useState(false);
  const cardRef = useRef<HTMLDivElement | null>(null);
  const credentialRef = useRef<HTMLInputElement | null>(null);
  const connected =
    info.enabled && (info.has_credential || !requiresCredential);
  // A provider with no curated rows and no custom ones yet has nothing to
  // summarize.
  const summary =
    catalogModels.length === 0
      ? null
      : `${catalogModels.length} model${catalogModels.length === 1 ? "" : "s"}`;

  const revealed = useRef(false);
  useEffect(() => {
    if (!deepLinked || revealed.current) return;
    revealed.current = true;
    // Optional-called: a headless renderer has no scroller, and failing to
    // scroll must not stop the card from opening.
    cardRef.current?.scrollIntoView?.({ block: "start", behavior: "smooth" });
    if (deepLinkFocusesCredential) setPendingCredentialFocus(true);
  }, [deepLinked, deepLinkFocusesCredential]);

  // The credential field only exists once the card is open, so focusing it is
  // deferred to the render that puts it on the page.
  useEffect(() => {
    if (!pendingCredentialFocus || !expanded) return;
    setPendingCredentialFocus(false);
    credentialRef.current?.focus();
  }, [expanded, pendingCredentialFocus]);

  function openWithCredentialFocus() {
    setPendingCredentialFocus(true);
    onExpandedChange(true);
  }

  async function save(enabled: boolean) {
    setSaving(true);
    setError(null);
    try {
      // Models save on their own from the Models section, so this write
      // leaves the custom list alone.
      const body: {
        enabled: boolean;
        base_url?: string | null;
        credential?: { type: "api_key"; key: string };
      } = { enabled };
      if (acceptsBaseUrl) {
        body.base_url = baseUrl.trim() || null;
      }
      if (key.trim()) {
        body.credential = { type: "api_key", key: key.trim() };
      }
      await client.putProvider(info.kind as ProviderKind, body);
      setKey("");
      onChanged();
      toast.success(`Saved ${providerLabel(info.kind)} settings`);
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  }

  async function clearCredential() {
    const accepted = await confirm({
      title: "Remove the saved credential?",
      description: `This deletes the stored ${providerLabel(
        info.kind,
      )} credential from the keychain on ${hostMachineLabel()}. You can add it again at any time.`,
      confirmLabel: "Remove credential",
      destructive: true,
    });
    if (!accepted) return;
    setSaving(true);
    setError(null);
    try {
      await client.deleteCredential(info.kind as ProviderKind);
      onChanged();
      toast.success(`Removed the saved ${providerLabel(info.kind)} credential`);
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  }

  const bodyId = `provider-card-${info.kind}`;
  return (
    <Card
      ref={cardRef}
      className="gap-0 overflow-hidden rounded-md border bg-transparent p-0 ring-0"
    >
      <div
        role="button"
        tabIndex={0}
        aria-expanded={expanded}
        aria-controls={bodyId}
        aria-label={`${expanded ? "Collapse" : "Expand"} ${providerLabel(info.kind)}`}
        className="flex cursor-pointer items-center gap-3 px-4 py-3 transition-colors hover:bg-muted/50"
        onClick={() => onExpandedChange(!expanded)}
        onKeyDown={(event) => {
          if (event.key !== "Enter" && event.key !== " ") return;
          event.preventDefault();
          onExpandedChange(!expanded);
        }}
      >
        <ChevronRight
          aria-hidden="true"
          className={cn(
            "size-4 shrink-0 text-muted-foreground transition-transform motion-reduce:transition-none",
            expanded && "rotate-90",
          )}
        />
        <ProviderIcon
          provider={info.kind}
          className={cn("size-4 shrink-0", !connected && "opacity-60")}
        />
        <span className="text-sm font-semibold">
          {providerLabel(info.kind)}
        </span>
        {summary && (
          <span className="text-xs text-muted-foreground">{summary}</span>
        )}
        <Badge
          variant={connected ? "success" : "outline"}
          size="sm"
          className="ml-auto"
        >
          {connected ? "Connected" : "Not connected"}
        </Badge>
        {!connected && (
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={(event) => {
              // The header is the toggle; a Set up click always opens rather
              // than toggling the card shut again.
              event.stopPropagation();
              openWithCredentialFocus();
            }}
          >
            Set up
          </Button>
        )}
      </div>
      {expanded && (
        <div
          id={bodyId}
          className="flex flex-col gap-4 border-t border-border p-4"
        >
          <div className="flex items-center justify-between gap-4">
            <div className="flex-1">
              <p className="text-sm font-bold">Enabled</p>
              <p className="text-xs text-muted-foreground">
                {credentialStatusLabel(info)}
              </p>
            </div>
            <Switch
              aria-label="Enabled"
              checked={info.enabled}
              disabled={saving}
              onCheckedChange={(checked) => void save(checked)}
            />
          </div>
          {info.kind === "xai" && (
            <p className="text-xs text-muted-foreground">
              Requests go directly to api.x.ai/v1.
            </p>
          )}
          {acceptsBaseUrl && (
            <Input
              type="text"
              aria-label="Base URL"
              placeholder={
                info.kind === "ollama"
                  ? "base URL (default http://127.0.0.1:11434/v1)"
                  : "base URL (e.g. http://127.0.0.1:1234/v1)"
              }
              value={baseUrl}
              onChange={(e) => setBaseUrl(e.target.value)}
            />
          )}
          {(info.kind === "fireworks" ||
            info.kind === "together" ||
            info.kind === "openrouter") &&
            info.base_url && (
              <p className="text-xs text-muted-foreground">
                Requests use the provider&apos;s fixed endpoint: {info.base_url}
              </p>
            )}
          {info.kind === "openai" ? (
            <OpenAiCredentialSection
              info={info}
              client={client}
              saving={saving}
              setSaving={setSaving}
              setError={setError}
              onChanged={onChanged}
              apiKey={key}
              setApiKey={setKey}
              apiKeyRef={credentialRef}
              onSaveApiKey={() => void save(true)}
              onClear={() => void clearCredential()}
            />
          ) : (
            <>
              <Input
                ref={credentialRef}
                type="password"
                placeholder={
                  info.kind === "ollama" ? "API key (optional)" : "API key"
                }
                value={key}
                onChange={(e) => setKey(e.target.value)}
                autoComplete="off"
              />
              <div className="flex flex-wrap gap-2">
                <Button
                  type="button"
                  disabled={
                    saving ||
                    (!info.has_credential &&
                      requiresCredential &&
                      info.kind !== "openai_compatible" &&
                      !key.trim())
                  }
                  onClick={() => void save(true)}
                >
                  Save configuration
                </Button>
                {info.has_credential && (
                  <Button
                    type="button"
                    variant="destructive"
                    disabled={saving}
                    onClick={() => void clearCredential()}
                  >
                    Clear
                  </Button>
                )}
              </div>
            </>
          )}
          {error && <SettingsError>{error}</SettingsError>}
          <ProviderModelsSection
            info={info}
            catalogModels={catalogModels}
            client={client}
            onChanged={onChanged}
          />
          {dialog}
        </div>
      )}
    </Card>
  );
}

function credentialStatusLabel(info: ProviderInfo): string {
  if (!info.has_credential) {
    return info.kind === "ollama"
      ? "No API key required for a local Ollama"
      : "No credential";
  }
  if (info.kind === "openai" && info.auth_mode === "chatgpt") {
    return "Signed in with ChatGPT";
  }
  if (info.kind === "openai" && info.auth_mode === "api_key") {
    return "API key set";
  }
  return "Credential set";
}

function OpenAiCredentialSection({
  info,
  client,
  saving,
  setSaving,
  setError,
  onChanged,
  apiKey,
  setApiKey,
  apiKeyRef,
  onSaveApiKey,
  onClear,
}: {
  info: ProviderInfo;
  client: ApiClient;
  saving: boolean;
  setSaving: (value: boolean) => void;
  setError: (value: string | null) => void;
  onChanged: () => void;
  apiKey: string;
  setApiKey: (value: string) => void;
  apiKeyRef: RefObject<HTMLInputElement | null>;
  onSaveApiKey: () => void;
  onClear: () => void;
}) {
  const signedInWithChatgpt =
    info.auth_mode === "chatgpt" && info.has_credential;
  const [pendingUrl, setPendingUrl] = useState<string | null>(null);
  const pollRef = useRef<number | null>(null);
  const pollDeadlineRef = useRef<number | null>(null);

  const stopPolling = useCallback(() => {
    if (pollRef.current !== null) {
      window.clearInterval(pollRef.current);
      pollRef.current = null;
    }
    if (pollDeadlineRef.current !== null) {
      window.clearTimeout(pollDeadlineRef.current);
      pollDeadlineRef.current = null;
    }
  }, []);

  useEffect(() => stopPolling, [stopPolling]);

  const startPolling = useCallback(
    (authorizationUrl: string | null) => {
      stopPolling();
      setPendingUrl(authorizationUrl);
      // The server dropped the attempt — an API key saved, credentials
      // cleared, or a newer sign-in started. Nothing failed, so stop without
      // reporting anything.
      const stopQuietly = () => {
        stopPolling();
        setPendingUrl(null);
      };
      const settle = (signedIn: boolean, failure?: string) => {
        stopQuietly();
        if (signedIn) {
          onChanged();
          toast.success("Signed in with ChatGPT");
          return;
        }
        setError(failure ?? "ChatGPT sign-in did not complete. Try again.");
        toast.error("ChatGPT sign-in failed");
      };
      pollRef.current = window.setInterval(() => {
        void client
          .getOpenaiChatgptStatus()
          .then((status) => {
            // Prefer a recorded failure over vault-only tokens: `signed_in`
            // requires the Oauth marker, but a half-finished persist can still
            // leave tokens while progress is Failed.
            if (status.error) settle(false, status.error);
            else if (status.signed_in) settle(true);
            else if (!status.pending_authorization_url) stopQuietly();
          })
          .catch(() => {
            /* transient; keep polling until the deadline below */
          });
      }, CHATGPT_SIGN_IN_POLL_MS);
      pollDeadlineRef.current = window.setTimeout(() => {
        stopPolling();
        // The server's own window has closed by now, so read the outcome it
        // recorded instead of leaving the row waiting on nothing.
        void client
          .getOpenaiChatgptStatus()
          .then((status) => {
            if (status.error) settle(false, status.error);
            else settle(status.signed_in);
          })
          .catch(() => settle(false));
      }, CHATGPT_SIGN_IN_TIMEOUT_MS);
    },
    [client, onChanged, setError, stopPolling],
  );

  // The sign-in runs on the server, so one started before this panel mounted —
  // or left behind when the user navigated away — is still waiting on the
  // browser. Pick it back up rather than sitting on a stale "no credential".
  const resumeChecked = useRef(false);
  useEffect(() => {
    if (resumeChecked.current || signedInWithChatgpt) return;
    resumeChecked.current = true;
    let dropped = false;
    void client
      .getOpenaiChatgptStatus()
      .then((status) => {
        if (dropped) return;
        if (status.error) {
          setError(status.error);
          return;
        }
        if (status.pending_authorization_url) {
          startPolling(status.pending_authorization_url);
        }
      })
      .catch(() => {
        /* the row still works without a resumed sign-in */
      });
    return () => {
      dropped = true;
    };
  }, [client, setError, signedInWithChatgpt, startPolling]);

  async function signInWithChatgpt() {
    setSaving(true);
    setError(null);
    try {
      const { authorization_url } = await client.openaiChatgptSignIn();
      await openInBrowser(authorization_url);
      toast.message("Finish signing in with ChatGPT in your browser");
      startPolling(authorization_url);
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  }

  async function signOutChatgpt() {
    setSaving(true);
    setError(null);
    try {
      await client.openaiChatgptSignOut();
      onChanged();
      toast.success("Signed out of ChatGPT");
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  }

  // Both auth modes stay reachable at once: signing in with ChatGPT replaces
  // an API key on the server, and saving a key signs ChatGPT out. Hiding one
  // path behind the other made "switch mode" look impossible.
  return (
    <>
      <p className="text-xs text-muted-foreground">
        Use a ChatGPT subscription (Plus / Pro) or an OpenAI Platform API key.
        Saving either one turns OpenAI on.
      </p>
      {signedInWithChatgpt ? (
        <div className="flex flex-wrap gap-2">
          <Button
            type="button"
            variant="destructive"
            disabled={saving}
            onClick={() => void signOutChatgpt()}
          >
            Sign out of ChatGPT
          </Button>
        </div>
      ) : (
        <div className="flex flex-wrap gap-2">
          <Button
            type="button"
            disabled={saving}
            onClick={() => void signInWithChatgpt()}
          >
            {info.auth_mode === "api_key"
              ? "Switch to ChatGPT sign-in"
              : "Sign in with ChatGPT"}
          </Button>
        </div>
      )}
      {pendingUrl && (
        <p className="text-xs text-muted-foreground">
          Waiting for the browser to finish signing in.{" "}
          <a
            href={pendingUrl}
            className="underline"
            onClick={(event) => {
              // No target="_blank": the shell plugin's injected click handler
              // opens such links itself without honoring preventDefault, which
              // doubles the tab. Route through the native opener and keep the
              // href for hover/copy.
              event.preventDefault();
              void openInBrowser(pendingUrl);
            }}
          >
            Open the sign-in page again
          </a>
        </p>
      )}
      <Input
        ref={apiKeyRef}
        type="password"
        placeholder={
          signedInWithChatgpt
            ? "Paste an API key to switch from ChatGPT"
            : info.auth_mode === "api_key"
              ? "Paste a new API key to replace"
              : "Or paste an API key"
        }
        value={apiKey}
        onChange={(e) => setApiKey(e.target.value)}
        autoComplete="off"
      />
      <div className="flex flex-wrap gap-2">
        <Button
          type="button"
          disabled={saving || !apiKey.trim()}
          onClick={onSaveApiKey}
        >
          {signedInWithChatgpt ? "Switch to API key" : "Save API key"}
        </Button>
        {info.has_credential && info.auth_mode === "api_key" && (
          <Button
            type="button"
            variant="destructive"
            disabled={saving}
            onClick={onClear}
          >
            Clear
          </Button>
        )}
      </div>
    </>
  );
}
