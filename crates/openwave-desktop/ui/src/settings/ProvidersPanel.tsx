import { useState } from "react";
import type { ApiClient, ProviderInfo, ProviderKind } from "../api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { SettingsError, SettingsPanel, SettingsSection } from "./primitives";

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
      description="Keys stay on this machine. Enable a provider, then save a credential."
    >
      {providers.map((p) => (
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
  const [baseUrl, setBaseUrl] = useState(info.base_url ?? "");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function save(enabled: boolean) {
    setSaving(true);
    setError(null);
    try {
      const body: {
        enabled: boolean;
        base_url?: string | null;
        credential?: { type: "api_key"; key: string };
      } = { enabled };
      if (info.kind === "openai_compatible") {
        body.base_url = baseUrl.trim() || null;
      }
      if (key.trim()) {
        body.credential = { type: "api_key", key: key.trim() };
      }
      await client.putProvider(info.kind as ProviderKind, body);
      setKey("");
      onChanged();
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
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  }

  return (
    <SettingsSection title={info.kind.replaceAll("_", " ")}>
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
        <Input
          type="text"
          placeholder="base URL (e.g. http://127.0.0.1:1234/v1)"
          value={baseUrl}
          onChange={(e) => setBaseUrl(e.target.value)}
        />
      )}
      <Input
        type="password"
        placeholder="API key"
        value={key}
        onChange={(e) => setKey(e.target.value)}
        autoComplete="off"
      />
      <div className="flex flex-wrap gap-2">
        <Button
          type="button"
          disabled={saving || !key.trim()}
          onClick={() => void save(true)}
        >
          Save
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
