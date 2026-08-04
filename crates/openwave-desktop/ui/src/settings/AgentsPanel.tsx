import { useEffect, useState } from "react";
import { toast } from "sonner";

import type { ApiClient } from "../api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  SettingsError,
  SettingsField,
  SettingsPanel,
  SettingsSection,
} from "./primitives";

const MIN_ACTIVE_AGENTS = 1;
const MAX_ACTIVE_AGENTS = 1024;

export function AgentsPanel({ client }: { client: ApiClient }) {
  const [limit, setLimit] = useState("");
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    void client
      .getSettings()
      .then((settings) => {
        if (!cancelled) setLimit(String(settings.max_active_background_agents));
      })
      .catch((err) => {
        if (!cancelled) setError(String(err));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [client]);

  async function save() {
    const parsed = Number(limit);
    if (
      !Number.isInteger(parsed) ||
      parsed < MIN_ACTIVE_AGENTS ||
      parsed > MAX_ACTIVE_AGENTS
    ) {
      setError(`Enter a whole number from ${MIN_ACTIVE_AGENTS} to ${MAX_ACTIVE_AGENTS}.`);
      return;
    }
    setSaving(true);
    setError(null);
    try {
      const settings = await client.putSettings({
        max_active_background_agents: parsed,
      });
      setLimit(String(settings.max_active_background_agents));
      toast.success("Saved agent settings");
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  }

  return (
    <SettingsPanel
      title="Agents"
      description="Control how much delegated background work one conversation may run at once."
      busy={loading}
    >
      <SettingsSection>
        <SettingsField
          label="Active background agents per chat"
          hint="A spawn beyond this limit fails immediately and can be retried after wait_for_agents returns."
        >
          <Input
            type="number"
            inputMode="numeric"
            min={MIN_ACTIVE_AGENTS}
            max={MAX_ACTIVE_AGENTS}
            step="1"
            value={limit}
            disabled={loading || saving}
            onChange={(event) => setLimit(event.target.value)}
          />
        </SettingsField>
        <Button type="button" disabled={loading || saving} onClick={() => void save()}>
          {saving ? "Saving…" : "Save settings"}
        </Button>
      </SettingsSection>
      {error && <SettingsError>{error}</SettingsError>}
    </SettingsPanel>
  );
}
