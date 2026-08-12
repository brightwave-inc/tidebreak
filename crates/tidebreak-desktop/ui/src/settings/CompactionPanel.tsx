import { useEffect, useState } from "react";
import { toast } from "sonner";

import type { ApiClient, CompactionSettings } from "../api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { friendlyErrorMessage } from "@/lib/utils";
import {
  SettingsError,
  SettingsField,
  SettingsPanel,
  SettingsSection,
} from "./primitives";

/** The form's own shape: percentages, because that is what the fields show. */
type CompactionForm = {
  thresholdPercent: string;
  targetPercent: string;
  protectRecent: string;
};

const MIN_PROTECT_RECENT = 1;
const MAX_PROTECT_RECENT = 100;

export function toCompactionForm(settings: CompactionSettings): CompactionForm {
  return {
    thresholdPercent: String(Math.round(settings.threshold_fraction * 100)),
    targetPercent: String(Math.round(settings.target_fraction * 100)),
    protectRecent: String(settings.protect_recent_messages),
  };
}

/**
 * The form as the settings API takes it, or the reason it cannot be sent.
 *
 * Both fractions are whole percentages here, which is a deliberate narrowing of
 * what the server accepts: nobody tunes a compaction threshold to a tenth of a
 * percent, and a percent field is legible where `0.75` is not. The
 * threshold-above-target rule is the server's, checked here as well so the
 * reader is told before the request rather than by it.
 */
export function compactionUpdateFrom(
  form: CompactionForm,
): { update: Partial<CompactionSettings> } | { error: string } {
  const threshold = Number(form.thresholdPercent);
  const target = Number(form.targetPercent);
  const protectRecent = Number(form.protectRecent);
  const percent = (value: number) => Number.isInteger(value) && value >= 1 && value <= 100;
  if (!percent(threshold) || !percent(target)) {
    return { error: "Enter whole percentages from 1 to 100." };
  }
  if (threshold <= target) {
    return {
      error: "The compaction point must be above what compaction leaves behind.",
    };
  }
  if (
    !Number.isInteger(protectRecent) ||
    protectRecent < MIN_PROTECT_RECENT ||
    protectRecent > MAX_PROTECT_RECENT
  ) {
    return {
      error: `Keep between ${MIN_PROTECT_RECENT} and ${MAX_PROTECT_RECENT} recent messages.`,
    };
  }
  return {
    update: {
      threshold_fraction: threshold / 100,
      target_fraction: target / 100,
      protect_recent_messages: protectRecent,
    },
  };
}

/**
 * When conversations compact themselves, and how much they keep.
 *
 * Host-global rather than per-chat: the cadence is a property of the install,
 * and a per-conversation copy would be one more thing to reason about on every
 * chat for a setting almost nobody changes twice. The one number worth putting
 * in front of a reader is where compaction starts; the rest is behind the
 * disclosure, because changing it badly makes conversations worse in ways that
 * are hard to attribute.
 */
export function CompactionPanel({ client }: { client: ApiClient }) {
  const [form, setForm] = useState<CompactionForm | null>(null);
  const [advanced, setAdvanced] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    void client
      .getSettings()
      .then((settings) => {
        if (!cancelled) setForm(toCompactionForm(settings.compaction));
      })
      .catch((caught) => {
        if (!cancelled) setError(friendlyErrorMessage(caught, "Could not read settings."));
      });
    return () => {
      cancelled = true;
    };
  }, [client]);

  async function save() {
    if (!form) return;
    const result = compactionUpdateFrom(form);
    if ("error" in result) {
      setError(result.error);
      return;
    }
    setSaving(true);
    setError(null);
    try {
      const settings = await client.putSettings({ compaction: result.update });
      setForm(toCompactionForm(settings.compaction));
      toast.success("Saved compaction settings");
    } catch (caught) {
      setError(friendlyErrorMessage(caught, "Could not save compaction settings."));
    } finally {
      setSaving(false);
    }
  }

  const field = (key: keyof CompactionForm) => ({
    value: form?.[key] ?? "",
    disabled: form === null || saving,
    onChange: (event: React.ChangeEvent<HTMLInputElement>) =>
      setForm((current) =>
        current ? { ...current, [key]: event.target.value } : current,
      ),
  });

  return (
    <SettingsPanel
      title="Context"
      description="How long conversations run before the agent summarizes what is behind them."
      busy={form === null}
    >
      <SettingsSection>
        <SettingsField
          label="Compact when the conversation reaches"
          hint="Percent of the model's context window. Below this, nothing is summarized. Type /compact in a chat to run it sooner."
        >
          <Input
            type="number"
            inputMode="numeric"
            min={1}
            max={100}
            step="1"
            aria-label="Compaction threshold, percent of the context window"
            {...field("thresholdPercent")}
          />
        </SettingsField>
        <div className="flex flex-col gap-4">
          <Button
            type="button"
            variant="ghost"
            className="self-start px-0 text-sm text-muted-foreground hover:bg-transparent"
            aria-expanded={advanced}
            onClick={() => setAdvanced((open) => !open)}
          >
            {advanced ? "Hide advanced" : "Advanced"}
          </Button>
          {advanced && (
            <>
              <SettingsField
                label="Compact down to"
                hint="Percent of the window the raw conversation is reduced to. The gap between this and the threshold is how long a chat runs before compacting again."
              >
                <Input
                  type="number"
                  inputMode="numeric"
                  min={1}
                  max={100}
                  step="1"
                  aria-label="Compaction target, percent of the context window"
                  {...field("targetPercent")}
                />
              </SettingsField>
              <SettingsField
                label="Recent messages kept in full"
                hint="Never summarized, however long the conversation gets."
              >
                <Input
                  type="number"
                  inputMode="numeric"
                  min={MIN_PROTECT_RECENT}
                  max={MAX_PROTECT_RECENT}
                  step="1"
                  aria-label="Recent messages kept in full"
                  {...field("protectRecent")}
                />
              </SettingsField>
            </>
          )}
        </div>
        <Button
          type="button"
          disabled={form === null || saving}
          onClick={() => void save()}
        >
          {saving ? "Saving…" : "Save settings"}
        </Button>
      </SettingsSection>
      {error && <SettingsError>{error}</SettingsError>}
    </SettingsPanel>
  );
}
