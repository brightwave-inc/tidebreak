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
const MIN_CHECKIN_STEPS = 1;
const MAX_CHECKIN_STEPS = 1000;
const MIN_ERROR_CHECKIN = 1;
const MAX_ERROR_CHECKIN = 100;

export function AgentsPanel({ client }: { client: ApiClient }) {
  const [limit, setLimit] = useState("");
  const [checkinSteps, setCheckinSteps] = useState("");
  const [errorCheckin, setErrorCheckin] = useState("");
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    void client
      .getSettings()
      .then((settings) => {
        if (cancelled) return;
        setLimit(String(settings.max_active_background_agents));
        setCheckinSteps(String(settings.sandbox_agent_checkin_steps));
        setErrorCheckin(String(settings.sandbox_agent_error_checkin));
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
    const parsedLimit = Number(limit);
    if (
      !Number.isInteger(parsedLimit) ||
      parsedLimit < MIN_ACTIVE_AGENTS ||
      parsedLimit > MAX_ACTIVE_AGENTS
    ) {
      setError(
        `Active agents must be a whole number from ${MIN_ACTIVE_AGENTS} to ${MAX_ACTIVE_AGENTS}.`,
      );
      return;
    }
    const parsedSteps = Number(checkinSteps);
    if (
      !Number.isInteger(parsedSteps) ||
      parsedSteps < MIN_CHECKIN_STEPS ||
      parsedSteps > MAX_CHECKIN_STEPS
    ) {
      setError(
        `Check-in steps must be a whole number from ${MIN_CHECKIN_STEPS} to ${MAX_CHECKIN_STEPS}.`,
      );
      return;
    }
    const parsedErrors = Number(errorCheckin);
    if (
      !Number.isInteger(parsedErrors) ||
      parsedErrors < MIN_ERROR_CHECKIN ||
      parsedErrors > MAX_ERROR_CHECKIN
    ) {
      setError(
        `Error check-in must be a whole number from ${MIN_ERROR_CHECKIN} to ${MAX_ERROR_CHECKIN}.`,
      );
      return;
    }
    setSaving(true);
    setError(null);
    try {
      const settings = await client.putSettings({
        max_active_background_agents: parsedLimit,
        sandbox_agent_checkin_steps: parsedSteps,
        sandbox_agent_error_checkin: parsedErrors,
      });
      setLimit(String(settings.max_active_background_agents));
      setCheckinSteps(String(settings.sandbox_agent_checkin_steps));
      setErrorCheckin(String(settings.sandbox_agent_error_checkin));
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
      description="Control how much delegated background work one conversation may run at once, and how often agents report back."
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
        <SettingsField
          label="Check in every N steps"
          hint="A step is one model turn, usually one tool call. Reaching the cadence never fails the agent — it wraps up what it has and reports back. Raising this rescues a running agent at its next step."
        >
          <Input
            type="number"
            inputMode="numeric"
            min={MIN_CHECKIN_STEPS}
            max={MAX_CHECKIN_STEPS}
            step="1"
            value={checkinSteps}
            disabled={loading || saving}
            onChange={(event) => setCheckinSteps(event.target.value)}
          />
        </SettingsField>
        <SettingsField
          label="Check in after N consecutive tool errors"
          hint="An agent whose tool calls keep failing reports back for direction instead of continuing to thrash. Any success resets the count."
        >
          <Input
            type="number"
            inputMode="numeric"
            min={MIN_ERROR_CHECKIN}
            max={MAX_ERROR_CHECKIN}
            step="1"
            value={errorCheckin}
            disabled={loading || saving}
            onChange={(event) => setErrorCheckin(event.target.value)}
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
