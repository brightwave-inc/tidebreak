import { useState } from "react";
import { toast } from "sonner";
import type {
  ApiClient,
  CustomModelConfig,
  ProviderInfo,
  ProviderKind,
} from "../api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { SettingsError, SettingsPanel, SettingsSection } from "./primitives";
import { providerLabel } from "../ModelSelection";

export function ProvidersPanel({
  providers,
  client,
  onChanged,
}: {
  providers: ProviderInfo[];
  client: ApiClient;
  onChanged: () => void;
}) {
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
      <div className="flex items-center justify-between gap-3">
        <label className="flex items-center gap-2 text-sm">
          <input
            type="checkbox"
            className="size-4 accent-[var(--primary)]"
            checked={info.enabled}
            disabled={saving}
            onChange={(e) => void save(e.target.checked)}
          />
          Enabled
        </label>
        <span className="text-xs text-muted-foreground">
          {info.has_credential ? "credential set" : "no credential"}
        </span>
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
      {info.kind === "gemini" && (
        <label className="grid gap-1 text-xs text-muted-foreground">
          Credential type
          <select
            aria-label="Gemini credential type"
            className="h-9 rounded-md border border-input bg-background px-3 text-sm text-foreground"
            value={credentialType}
            disabled={saving}
            onChange={(event) =>
              setCredentialType(
                event.target.value as "api_key" | "service_account",
              )
            }
          >
            <option value="api_key">Gemini API key</option>
            <option value="service_account">Google Cloud service account</option>
          </select>
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
          <textarea
            aria-label="Google service account JSON"
            className="min-h-28 w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-sm text-foreground"
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
            variant="outline"
            disabled={saving}
            onClick={() => void clearCredential()}
          >
            Clear
          </Button>
        )}
      </div>
      {error && <SettingsError>{error}</SettingsError>}
    </SettingsSection>
  );
}
