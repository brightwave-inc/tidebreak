import { useCallback, useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import type {
  ApiClient,
  CustomModelConfig,
  ProviderInfo,
  ProviderKind,
} from "../api";
import { openSignInPage } from "../openSignInPage";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import { useConfirm } from "../components/ConfirmDialog";
import { SettingsError, SettingsPanel, SettingsSection } from "./primitives";
import { providerLabel } from "../ModelSelection";

const CHATGPT_SIGN_IN_POLL_MS = 2_000;
// Matches the server's sign-in window; polling past it can only report a
// timeout the server has already recorded.
const CHATGPT_SIGN_IN_TIMEOUT_MS = 5 * 60 * 1000;

export function ProvidersPanel({
  providers,
  client,
  managed = false,
  onChanged,
}: {
  providers: ProviderInfo[];
  client: ApiClient;
  /** A managed profile's models all come from its gateway, and the server
   * refuses every credential and endpoint write here — so the editors are not
   * shown at all rather than offered and then rejected. */
  managed?: boolean;
  onChanged: () => void;
}) {
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
      {providers
        // The gateway signs in with OAuth, not a pasted key; its whole
        // surface (connect, identity, entitled models) lives in the
        // dedicated Model Gateway settings panel.
        .filter((p) => p.kind !== "model_gateway")
        .map((p) => (
          <ProviderRow key={p.kind} info={p} client={client} onChanged={onChanged} />
        ))}
    </SettingsPanel>
  );
}

function ProviderRow({
  info,
  client,
  onChanged,
}: {
  info: ProviderInfo;
  client: ApiClient;
  onChanged: () => void;
}) {
  const [key, setKey] = useState("");
  const [credentialType, setCredentialType] = useState<
    "api_key" | "service_account"
  >("api_key");
  const [serviceAccountJson, setServiceAccountJson] = useState("");
  const [vertexLocation, setVertexLocation] = useState(
    info.vertex_location ?? "global",
  );
  const [baseUrl, setBaseUrl] = useState(info.base_url ?? "");
  const [models, setModels] = useState<CustomModelConfig[]>(info.models);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const { confirm, dialog } = useConfirm();

  async function save(enabled: boolean) {
    setSaving(true);
    setError(null);
    try {
      const body: {
        enabled: boolean;
        base_url?: string | null;
        vertex_location?: string | null;
        credential?:
          | { type: "api_key"; key: string }
          | { type: "service_account"; json: string };
        models?: CustomModelConfig[];
      } = { enabled };
      if (info.kind === "openai_compatible") {
        body.base_url = baseUrl.trim() || null;
        body.models = models.map((model) => ({
          ...model,
          id: model.id.trim(),
          // Omitted rather than null, which is how the server represents an
          // unset display name and what it sends back. `models` is a full
          // replacement list, so an absent key clears it just as null did.
          display_name: model.display_name?.trim() || undefined,
        }));
      }
      if (key.trim()) {
        body.credential = { type: "api_key", key: key.trim() };
      }
      if (info.kind === "gemini" && credentialType === "service_account") {
        body.vertex_location = vertexLocation.trim() || "global";
        if (serviceAccountJson.trim()) {
          body.credential = {
            type: "service_account",
            json: serviceAccountJson.trim(),
          };
        }
      }
      await client.putProvider(info.kind as ProviderKind, body);
      setKey("");
      setServiceAccountJson("");
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

  return (
    <SettingsSection title={providerLabel(info.kind)}>
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
      {info.kind === "openai_compatible" && (
        <>
          <Input
            type="text"
            placeholder="base URL (e.g. http://127.0.0.1:1234/v1)"
            value={baseUrl}
            onChange={(e) => setBaseUrl(e.target.value)}
          />
          <div className="space-y-2">
            <div className="flex items-center justify-between gap-3">
              <span className="text-sm font-medium">Models</span>
              <Button
                type="button"
                variant="outline"
                disabled={saving}
                onClick={() =>
                  setModels((current) => [
                    ...current,
                    {
                      id: "",
                      context_window: 32_768,
                      max_output_tokens: 4_096,
                    },
                  ])
                }
              >
                Add model
              </Button>
            </div>
            {models.length === 0 && (
              <p className="text-xs text-muted-foreground">
                Add each model this endpoint serves. Custom models start with
                conservative text-only, non-reasoning capabilities.
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
          onSaveApiKey={() => void save(true)}
          onClear={() => void clearCredential()}
        />
      ) : (
        <>
      {info.kind === "gemini" && (
        <label className="grid gap-1 text-xs text-muted-foreground">
          Credential type
          <Select
            value={credentialType}
            disabled={saving}
            onValueChange={(value) =>
              setCredentialType(value as "api_key" | "service_account")
            }
          >
            <SelectTrigger aria-label="Gemini credential type" className="h-9">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="api_key">Gemini API key</SelectItem>
              <SelectItem value="service_account">
                Google Cloud service account
              </SelectItem>
            </SelectContent>
          </Select>
        </label>
      )}
      {credentialType === "api_key" && (
        <Input
          type="password"
          placeholder="API key"
          value={key}
          onChange={(e) => setKey(e.target.value)}
          autoComplete="off"
        />
      )}
      {info.kind === "gemini" && credentialType === "service_account" && (
        <>
          <Input
            type="text"
            aria-label="Vertex AI location"
            placeholder="Vertex AI location"
            value={vertexLocation}
            onChange={(event) => setVertexLocation(event.target.value)}
            autoComplete="off"
          />
          <p className="text-xs text-muted-foreground">
            Gemini 3 models always use Google&apos;s global endpoint. This
            location applies to models that support regional Vertex endpoints.
          </p>
          <Textarea
            aria-label="Google service account JSON"
            className="min-h-28 font-mono text-sm text-foreground"
            placeholder="Paste the Google service-account JSON key file"
            value={serviceAccountJson}
            onChange={(event) => setServiceAccountJson(event.target.value)}
            autoComplete="off"
            spellCheck={false}
          />
        </>
      )}
      <div className="flex flex-wrap gap-2">
        <Button
          type="button"
          disabled={
            saving ||
            (!info.has_credential &&
              credentialType === "api_key" &&
              info.kind !== "openai_compatible" &&
              !key.trim()) ||
            (!info.has_credential &&
              info.kind === "gemini" &&
              credentialType === "service_account" &&
              !serviceAccountJson.trim()) ||
            (info.kind === "openai_compatible" &&
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
      {dialog}
    </SettingsSection>
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
      const settle = (signedIn: boolean, failure?: string) => {
        stopPolling();
        setPendingUrl(null);
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
            if (status.error) settle(false, status.error);
            else if (status.signed_in) settle(true);
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
          .then((status) => settle(status.signed_in, status.error))
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
