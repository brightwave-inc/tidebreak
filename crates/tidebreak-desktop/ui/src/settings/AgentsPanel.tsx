import { useEffect, useState } from "react";
import { toast } from "sonner";

import type { ApiClient } from "../api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import { Switch } from "@/components/ui/switch";
import { useUiStore, type ActiveTurnSendMode } from "../UiStore";
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
  const activeTurnSendMode = useUiStore((state) => state.activeTurnSendMode);
  const setActiveTurnSendMode = useUiStore(
    (state) => state.setActiveTurnSendMode,
  );
  const [limit, setLimit] = useState("");
  const [checkinSteps, setCheckinSteps] = useState("");
  const [errorCheckin, setErrorCheckin] = useState("");
  const [turnRecapsEnabled, setTurnRecapsEnabled] = useState(true);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [savingTurnRecaps, setSavingTurnRecaps] = useState(false);
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
        setTurnRecapsEnabled(settings.code_turn_recaps_enabled);
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

  async function saveTurnRecaps(enabled: boolean) {
    const previous = turnRecapsEnabled;
    setTurnRecapsEnabled(enabled);
    setSavingTurnRecaps(true);
    setError(null);
    try {
      const settings = await client.putSettings({
        code_turn_recaps_enabled: enabled,
      });
      setTurnRecapsEnabled(settings.code_turn_recaps_enabled);
    } catch (err) {
      setTurnRecapsEnabled(previous);
      setError(String(err));
    } finally {
      setSavingTurnRecaps(false);
    }
  }

  return (
    <SettingsPanel
      title="Agents"
      description="Choose how messages behave around active work, whether coding turns write recaps, and how delegated agents report back."
      busy={loading}
    >
      <SettingsSection
        title="While an agent is responding"
        description="Choose what the single composer action does when you type during a running response."
      >
        <RadioGroup
          aria-label="Default action while an agent is responding"
          value={activeTurnSendMode}
          onValueChange={(value) =>
            setActiveTurnSendMode(value as ActiveTurnSendMode)
          }
          className="gap-3"
        >
          <label className="flex cursor-pointer items-start gap-3 rounded-lg border border-border/70 p-3 transition-colors hover:bg-muted/40">
            <RadioGroupItem
              value="queue"
              className="mt-0.5"
              aria-label="Queue"
            />
            <span className="flex flex-col gap-0.5">
              <span className="text-sm font-medium">Queue</span>
              <span className="text-sm text-muted-foreground">
                Run the message as its own turn after the current response.
              </span>
            </span>
          </label>
          <label className="flex cursor-pointer items-start gap-3 rounded-lg border border-border/70 p-3 transition-colors hover:bg-muted/40">
            <RadioGroupItem
              value="steer"
              className="mt-0.5"
              aria-label="Steer"
            />
            <span className="flex flex-col gap-0.5">
              <span className="text-sm font-medium">Steer</span>
              <span className="text-sm text-muted-foreground">
                Interrupt the current response and use the message as guidance.
              </span>
            </span>
          </label>
        </RadioGroup>
      </SettingsSection>
      <SettingsSection
        title="Turn recaps"
        description="Tidebreak keeps Claude Code's captured recap. For other engines, it can add a one-line fallback after the turn finishes."
      >
        <SettingsField
          label="Write fallback recaps"
          hint="Uses the utility model when the engine does not supply a recap. Turning this off stops future fallback recaps; existing recaps stay in the transcript."
        >
          <Switch
            checked={turnRecapsEnabled}
            disabled={loading || savingTurnRecaps}
            onCheckedChange={(enabled) => void saveTurnRecaps(enabled)}
            aria-label="Write fallback recaps"
          />
        </SettingsField>
      </SettingsSection>
      <SettingsSection>
        <SettingsField
          label="Active background agents per work"
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
        <Button
          type="button"
          disabled={loading || saving}
          onClick={() => void save()}
        >
          {saving ? "Saving…" : "Save settings"}
        </Button>
      </SettingsSection>
      {error && <SettingsError>{error}</SettingsError>}
    </SettingsPanel>
  );
}
