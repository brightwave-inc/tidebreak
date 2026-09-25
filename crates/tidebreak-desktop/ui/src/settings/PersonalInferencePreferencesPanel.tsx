import { useEffect, useRef, useState } from "react";
import type { ApiClient } from "@/api";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  SettingsError,
  SettingsField,
  SettingsPanel,
  SettingsSection,
} from "./primitives";
import {
  CHANNEL_SUBSCRIPTION_CONSENT,
  SUBSCRIPTION_PREFERENCES_UNAVAILABLE,
  type DmSubscriptionPreference,
  type PersonalInferencePreferences,
} from "./inferencePreferences";
import { Notice } from "@/components/ui/notice";
import { friendlyErrorMessage } from "@/lib/utils";

export function PersonalInferencePreferencesPanel({
  client,
  grantId,
  onBack,
}: {
  client: ApiClient;
  grantId: string;
  onBack: () => void;
}) {
  const [preferences, setPreferences] =
    useState<PersonalInferencePreferences | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);
  const generation = useRef(0);
  useEffect(() => {
    const current = ++generation.current;
    setLoading(true);
    setSaving(false);
    setPreferences(null);
    setError(null);
    void client
      .getPersonalInferencePreferences(grantId)
      .then((value) => {
        if (generation.current === current) setPreferences(value);
      })
      .catch((error: unknown) => {
        if (generation.current === current) {
          setError(friendlyErrorMessage(error, "Try again in a moment."));
        }
      })
      .finally(() => {
        if (generation.current === current) setLoading(false);
      });
    return () => {
      generation.current++;
    };
  }, [client, grantId, attempt]);

  async function save(
    changes: Partial<
      Pick<
        PersonalInferencePreferences,
        "dm_subscription_preference" | "channel_sponsorship_enabled"
      >
    >,
  ) {
    if (!preferences?.inference_sponsorship_supported || saving) return;
    const current = generation.current;
    const next = { ...preferences, ...changes };
    setSaving(true);
    setError(null);
    try {
      const saved = await client.setPersonalInferencePreferences(grantId, {
        dm_subscription_preference: next.dm_subscription_preference,
        channel_sponsorship_enabled: next.channel_sponsorship_enabled,
        consent_version: next.channel_sponsorship_enabled ? 1 : null,
      });
      if (generation.current === current) setPreferences(saved);
    } catch (error) {
      if (generation.current === current) {
        setError(friendlyErrorMessage(error, "Could not save that change."));
      }
    } finally {
      if (generation.current === current) setSaving(false);
    }
  }

  const disabled = saving || !preferences?.inference_sponsorship_supported;
  return (
    <SettingsPanel
      title="Your Slack subscriptions"
      description="Choose how this Slack connection uses subscriptions you own in Gateway. Only you can change these settings."
      busy={loading || saving}
    >
      <Button variant="ghost" className="self-start" onClick={onBack}>
        Back to Channels
      </Button>
      {loading ? (
        <p className="text-sm text-muted-foreground" role="status">
          Loading subscription settings…
        </p>
      ) : !preferences ? (
        <SettingsError
          title="Could not load your subscription settings"
          onRetry={() => setAttempt((value) => value + 1)}
        >
          {error}
        </SettingsError>
      ) : (
        <>
          {!preferences.inference_sponsorship_supported && (
            <Notice tone="info">{SUBSCRIPTION_PREFERENCES_UNAVAILABLE}</Notice>
          )}
          <SettingsSection title="Direct messages">
            <SettingsField
              label="DM subscription preference"
              hint="Apply this choice to new direct-message conversations. If none of your subscriptions is eligible, the conversation uses Gateway defaults."
            >
              <Select
                value={preferences.dm_subscription_preference}
                disabled={disabled}
                onValueChange={(value) =>
                  void save({
                    dm_subscription_preference:
                      value as DmSubscriptionPreference,
                  })
                }
              >
                <SelectTrigger aria-label="DM subscription preference">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="prefer_owned_subscription">
                    Prefer my subscription
                  </SelectItem>
                  <SelectItem value="gateway_default">
                    Use Gateway defaults
                  </SelectItem>
                </SelectContent>
              </Select>
            </SettingsField>
          </SettingsSection>
          <SettingsSection title="Channel conversations">
            <SettingsField
              label="Use my subscriptions in channels"
              hint={CHANNEL_SUBSCRIPTION_CONSENT}
            >
              <Switch
                aria-label="Use my subscriptions in channels"
                disabled={disabled}
                checked={preferences.channel_sponsorship_enabled}
                onCheckedChange={(enabled) =>
                  void save({ channel_sponsorship_enabled: enabled })
                }
              />
            </SettingsField>
            <p className="text-sm text-muted-foreground">
              Turning this off stops further use of your subscriptions in
              conversations you already sponsor. Those conversations pause
              instead of changing who pays. Turning it on applies to new
              conversations; it does not change an existing conversation’s
              subscription choice.
            </p>
            <p className="text-sm text-muted-foreground">
              Channel settings can choose Gateway defaults. Your subscription
              does not give the agent more repository, model, or tool access.
            </p>
          </SettingsSection>
          {error && <SettingsError>{error}</SettingsError>}
          {saving && (
            <p className="text-sm text-muted-foreground" role="status">
              Saving…
            </p>
          )}
        </>
      )}
    </SettingsPanel>
  );
}
