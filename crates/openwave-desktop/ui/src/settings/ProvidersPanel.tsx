import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { RefObject } from "react";
import { ChevronRight } from "lucide-react";
import { toast } from "sonner";
import type {
  ApiClient,
  CustomModelConfig,
  ModelInfo,
  ProviderInfo,
  ProviderKind,
  ReasoningEffort,
} from "../api";
import { openSignInPage } from "../openSignInPage";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { cn, friendlyErrorMessage } from "@/lib/utils";
import { useConfirm } from "../components/ConfirmDialog";
import {
  isModelVisible,
  type ModelVisibilityOverrides,
} from "../modelVisibility";
import { SettingsError, SettingsPanel, SettingsSection } from "./primitives";
import { ProviderIcon } from "../ProviderIcons";
import { providerLabel } from "../ModelSelection";

const CHATGPT_SIGN_IN_POLL_MS = 2_000;
// Matches the server's sign-in window; polling past it can only report a
// timeout the server has already recorded.
const CHATGPT_SIGN_IN_TIMEOUT_MS = 5 * 60 * 1000;
const XAI_REASONING_EFFORTS: readonly ReasoningEffort[] = [
  "none",
  "low",
  "medium",
  "high",
  "xhigh",
];

function newConfiguredModel(): CustomModelConfig {
  return {
    id: "",
    context_window: 32_768,
    max_output_tokens: 4_096,
    input_modalities: ["text"],
    supports_reasoning: false,
    reasoning_efforts: [],
  };
}

const EXPANDED_PROVIDERS_KEY = "openwave.settings.providers-expanded";

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
  const [overrides, setOverrides] = useState<ModelVisibilityOverrides>({});

  // Visibility is a reader preference stored in settings, not part of the
  // provider catalog, so the panel fetches it once for every card. A failure
  // leaves the checklists showing catalog defaults, which is what a reader with
  // no overrides sees anyway.
  useEffect(() => {
    let dropped = false;
    void Promise.resolve()
      .then(() => client.getSettings())
      .then((settings) => {
        if (!dropped) setOverrides(settings.model_visibility_overrides ?? {});
      })
      .catch(() => {
        /* the cards still work against the catalog defaults */
      });
    return () => {
      dropped = true;
    };
  }, [client]);

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
        description="This OpenWave is managed by your organization."
      >
        <SettingsSection>
          <p className="text-sm text-muted-foreground">
            Model providers are configured by your organization&apos;s model
            gateway. Your own API keys and endpoints are not used, and cannot
            be added here. See the Model Gateway section for the models and
            tools available to you.
          </p>
        </SettingsSection>
      </SettingsPanel>
    );
  }
  return (
    <SettingsPanel
      title="Providers"
      description="Credentials stay on this machine. Enable a provider, then save a credential."
    >
      <div className="flex flex-col gap-3">
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
              catalogModels={models.filter((model) => model.provider === p.kind)}
              overrides={overrides}
              onOverridesChanged={setOverrides}
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
  overrides,
  onOverridesChanged,
  expanded,
  onExpandedChange,
  deepLinked,
  deepLinkFocusesCredential,
}: {
  info: ProviderInfo;
  client: ApiClient;
  onChanged: () => void;
  catalogModels: ModelInfo[];
  overrides: ModelVisibilityOverrides;
  onOverridesChanged: (next: ModelVisibilityOverrides) => void;
  expanded: boolean;
  onExpandedChange: (open: boolean) => void;
  deepLinked: boolean;
  deepLinkFocusesCredential: boolean;
}) {
  const [key, setKey] = useState("");
  const [baseUrl, setBaseUrl] = useState(info.base_url ?? "");
  const [models, setModels] = useState<CustomModelConfig[]>(info.models);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const { confirm, dialog } = useConfirm();
  const hasConfigurableModels =
    info.kind === "openai_compatible" || info.kind === "xai";
  const [visibilitySaving, setVisibilitySaving] = useState(false);
  const [pendingCredentialFocus, setPendingCredentialFocus] = useState(false);
  const cardRef = useRef<HTMLDivElement | null>(null);
  const credentialRef = useRef<HTMLInputElement | null>(null);
  const connected = info.has_credential && info.enabled;
  const visibleCount = useMemo(
    () =>
      catalogModels.filter((model) => isModelVisible(model, overrides)).length,
    [catalogModels, overrides],
  );
  // A provider whose models are all user-configured — or served by a gateway —
  // has no curated catalog rows here, so there is nothing to summarize or list.
  const summary =
    catalogModels.length === 0
      ? null
      : connected
        ? `${visibleCount} of ${catalogModels.length} models shown`
        : `${catalogModels.length} models`;

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

  /**
   * Persist the reader's visibility choices. The map is deviations only and the
   * server replaces it wholesale, so every write sends the complete desired set
   * — and a model returned to its catalog default loses its entry rather than
   * gaining a redundant one.
   */
  async function writeOverrides(
    next: ModelVisibilityOverrides,
    failure: string,
  ) {
    setVisibilitySaving(true);
    try {
      const settings = await client.putSettings({
        model_visibility_overrides: next,
      });
      onOverridesChanged(settings.model_visibility_overrides ?? {});
      // The shell holds its own copy of the overrides for the picker, so a
      // write here is a catalog change as much as a credential save is.
      onChanged();
    } catch (err) {
      toast.error(friendlyErrorMessage(err, failure));
    } finally {
      setVisibilitySaving(false);
    }
  }

  async function setModelVisible(model: ModelInfo, visible: boolean) {
    const next = { ...overrides };
    if (visible === model.recommended) delete next[model.key];
    else next[model.key] = visible ? "show" : "hide";
    await writeOverrides(
      next,
      `Could not update which ${providerLabel(info.kind)} models the picker shows.`,
    );
  }

  async function resetToRecommended() {
    const owned = new Set<string>(catalogModels.map((model) => model.key));
    const next = Object.fromEntries(
      Object.entries(overrides).filter(([key]) => !owned.has(key)),
    ) as ModelVisibilityOverrides;
    await writeOverrides(
      next,
      `Could not reset the ${providerLabel(info.kind)} models.`,
    );
  }

  function openWithCredentialFocus() {
    setPendingCredentialFocus(true);
    onExpandedChange(true);
  }

  async function save(enabled: boolean) {
    setSaving(true);
    setError(null);
    try {
      const body: {
        enabled: boolean;
        base_url?: string | null;
        credential?: { type: "api_key"; key: string };
        models?: CustomModelConfig[];
      } = { enabled };
      if (hasConfigurableModels) {
        if (info.kind === "openai_compatible") {
          body.base_url = baseUrl.trim() || null;
        }
        body.models = models.map((model) => ({
          ...model,
          id: model.id.trim(),
          // Omitted rather than null, which is how the server represents an
          // unset display name and what it sends back. `models` is a full
          // replacement list, so an absent key clears it just as null did.
          display_name: model.display_name?.trim() || undefined,
          input_modalities: model.input_modalities ?? ["text"],
          supports_reasoning: model.supports_reasoning ?? false,
          reasoning_efforts: model.reasoning_efforts ?? [],
        }));
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
      )} credential from this machine's keychain. You can add it again at any time.`,
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
      className="gap-0 overflow-hidden border bg-transparent p-0"
    >
      <div
        role="button"
        tabIndex={0}
        aria-expanded={expanded}
        aria-controls={bodyId}
        aria-label={`${expanded ? "Collapse" : "Expand"} ${providerLabel(info.kind)}`}
        className="flex cursor-pointer items-center gap-3 px-4 py-3 hover:bg-muted/40"
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
        <span className="text-sm font-semibold">{providerLabel(info.kind)}</span>
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
          {hasConfigurableModels && (
            <>
              {info.kind === "openai_compatible" && (
                <Input
                  type="text"
                  placeholder="base URL (e.g. http://127.0.0.1:1234/v1)"
                  value={baseUrl}
                  onChange={(e) => setBaseUrl(e.target.value)}
                />
              )}
              {info.kind === "xai" && (
                <p className="text-xs text-muted-foreground">
                  Requests go directly to api.x.ai/v1.
                </p>
              )}
              <div className="space-y-2">
                <div className="flex items-center justify-between gap-3">
                  <span className="text-sm font-medium">Models</span>
                  <Button
                    type="button"
                    variant="outline"
                    disabled={saving}
                    onClick={() =>
                      setModels((current) => [...current, newConfiguredModel()])
                    }
                  >
                    Add model
                  </Button>
                </div>
                {models.length === 0 && (
                  <p className="text-xs text-muted-foreground">
                    {info.kind === "xai"
                      ? "Grok 4.5 is ready when you connect xAI. Add any additional account models here, including their limits and supported capabilities."
                      : "Add each model this endpoint serves. Custom models start with conservative text-only, non-reasoning capabilities."}
                  </p>
                )}
                {models.map((model, index) => (
                  <div
                    className="grid gap-2 rounded-md border border-border p-3"
                    key={index}
                  >
                    <Input
                      type="text"
                      aria-label={`Custom model ${index + 1} ID`}
                      placeholder="model ID"
                      value={model.id}
                      onChange={(event) =>
                        setModels((current) =>
                          current.map((item, itemIndex) =>
                            itemIndex === index
                              ? { ...item, id: event.target.value }
                              : item,
                          ),
                        )
                      }
                    />
                    <Input
                      type="text"
                      aria-label={`Custom model ${index + 1} display name`}
                      placeholder="display name (optional)"
                      value={model.display_name ?? ""}
                      onChange={(event) =>
                        setModels((current) =>
                          current.map((item, itemIndex) =>
                            itemIndex === index
                              ? {
                                  ...item,
                                  display_name: event.target.value || undefined,
                                }
                              : item,
                          ),
                        )
                      }
                    />
                    <div className="grid grid-cols-2 gap-2">
                      <label className="grid gap-1 text-xs text-muted-foreground">
                        Context tokens
                        <Input
                          type="number"
                          min={1024}
                          aria-label={`Custom model ${index + 1} context tokens`}
                          value={model.context_window}
                          onChange={(event) =>
                            setModels((current) =>
                              current.map((item, itemIndex) =>
                                itemIndex === index
                                  ? {
                                      ...item,
                                      context_window: Number(event.target.value),
                                    }
                                  : item,
                              ),
                            )
                          }
                        />
                      </label>
                      <label className="grid gap-1 text-xs text-muted-foreground">
                        Max output
                        <Input
                          type="number"
                          min={1}
                          aria-label={`Custom model ${index + 1} max output`}
                          value={model.max_output_tokens}
                          onChange={(event) =>
                            setModels((current) =>
                              current.map((item, itemIndex) =>
                                itemIndex === index
                                  ? {
                                      ...item,
                                      max_output_tokens: Number(event.target.value),
                                    }
                                  : item,
                              ),
                            )
                          }
                        />
                      </label>
                    </div>
                    {info.kind === "xai" && (
                      <div className="grid gap-2 rounded-md bg-muted/40 p-2 text-xs">
                        <label className="flex items-center gap-2">
                          <Checkbox
                            checked={
                              model.input_modalities?.includes("image") ?? false
                            }
                            disabled={saving}
                            onCheckedChange={(checked) =>
                              setModels((current) =>
                                current.map((item, itemIndex) =>
                                  itemIndex === index
                                    ? {
                                        ...item,
                                        input_modalities:
                                          checked === true
                                            ? ["text", "image"]
                                            : ["text"],
                                      }
                                    : item,
                                ),
                              )
                            }
                          />
                          Image input
                        </label>
                        <label className="flex items-center gap-2">
                          <Checkbox
                            checked={model.supports_reasoning ?? false}
                            disabled={saving}
                            onCheckedChange={(checked) =>
                              setModels((current) =>
                                current.map((item, itemIndex) =>
                                  itemIndex === index
                                    ? {
                                        ...item,
                                        supports_reasoning: checked === true,
                                        reasoning_efforts:
                                          checked === true
                                            ? item.reasoning_efforts ?? []
                                            : [],
                                      }
                                    : item,
                                ),
                              )
                            }
                          />
                          Reasoning model
                        </label>
                        {model.supports_reasoning && (
                          <div className="grid gap-1">
                            <span className="text-muted-foreground">
                              Supported reasoning efforts
                            </span>
                            <div className="flex flex-wrap gap-3">
                              {XAI_REASONING_EFFORTS.map((effort) => (
                                <label
                                  className="flex items-center gap-1.5"
                                  key={effort}
                                >
                                  <Checkbox
                                    checked={
                                      model.reasoning_efforts?.includes(effort) ??
                                      false
                                    }
                                    disabled={saving}
                                    onCheckedChange={(checked) =>
                                      setModels((current) =>
                                        current.map((item, itemIndex) => {
                                          if (itemIndex !== index) return item;
                                          const selected = new Set(
                                            item.reasoning_efforts ?? [],
                                          );
                                          if (checked === true) selected.add(effort);
                                          else selected.delete(effort);
                                          return {
                                            ...item,
                                            reasoning_efforts: XAI_REASONING_EFFORTS.filter(
                                              (candidate) => selected.has(candidate),
                                            ),
                                          };
                                        }),
                                      )
                                    }
                                  />
                                  {effort}
                                </label>
                              ))}
                            </div>
                          </div>
                        )}
                      </div>
                    )}
                    <Button
                      type="button"
                      variant="outline"
                      disabled={saving}
                      onClick={() =>
                        setModels((current) =>
                          current.filter((_, itemIndex) => itemIndex !== index),
                        )
                      }
                    >
                      Remove model
                    </Button>
                  </div>
                ))}
              </div>
            </>
          )}
          {(info.kind === "fireworks" || info.kind === "together") &&
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
                placeholder="API key"
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
                      info.kind !== "openai_compatible" &&
                      !key.trim()) ||
                    (hasConfigurableModels &&
                      models.some((model) => !model.id.trim()))
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
          {catalogModels.length > 0 && (
            <div className="flex flex-col gap-1 border-t border-border pt-4">
              <div className="flex items-baseline justify-between gap-3">
                <h3 className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">
                  Models in the picker
                </h3>
                <Button
                  type="button"
                  variant="link"
                  className="h-auto p-0 text-xs"
                  disabled={visibilitySaving}
                  onClick={() => void resetToRecommended()}
                >
                  Reset to defaults
                </Button>
              </div>
              {catalogModels.map((model) => {
                const visible = isModelVisible(model, overrides);
                return (
                  <label
                    key={model.key}
                    className="flex items-center gap-2 rounded-md px-1 py-1.5 text-sm hover:bg-muted/40"
                  >
                    <Checkbox
                      checked={visible}
                      disabled={visibilitySaving}
                      aria-label={`Show ${model.display_name}`}
                      onCheckedChange={(checked) =>
                        void setModelVisible(model, checked === true)
                      }
                    />
                    <span className={cn(!visible && "text-muted-foreground")}>
                      {model.display_name}
                    </span>
                  </label>
                );
              })}
              <p className="pt-2 text-xs text-muted-foreground">
                Hidden models stay usable in chats that already selected them.
              </p>
            </div>
          )}
          {dialog}
        </div>
      )}
    </Card>
  );
}

function credentialStatusLabel(info: ProviderInfo): string {
  if (!info.has_credential) return "No credential";
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
  const signedInWithChatgpt = info.auth_mode === "chatgpt" && info.has_credential;
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
      await openSignInPage(authorization_url);
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

  if (signedInWithChatgpt) {
    return (
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
    );
  }

  return (
    <>
      <p className="text-xs text-muted-foreground">
        Use a ChatGPT subscription (Plus / Pro) or an OpenAI Platform API key.
      </p>
      <div className="flex flex-wrap gap-2">
        <Button
          type="button"
          disabled={saving}
          onClick={() => void signInWithChatgpt()}
        >
          Sign in with ChatGPT
        </Button>
      </div>
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
              void openSignInPage(pendingUrl);
            }}
          >
            Open the sign-in page again
          </a>
        </p>
      )}
      <Input
        ref={apiKeyRef}
        type="password"
        placeholder="Or paste an API key"
        value={apiKey}
        onChange={(e) => setApiKey(e.target.value)}
        autoComplete="off"
      />
      <div className="flex flex-wrap gap-2">
        <Button
          type="button"
          disabled={saving || (!info.has_credential && !apiKey.trim())}
          onClick={onSaveApiKey}
        >
          Save configuration
        </Button>
        {info.has_credential && (
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
