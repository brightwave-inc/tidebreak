export type DmSubscriptionPreference =
  | "prefer_owned_subscription"
  | "gateway_default";
export type InferenceSponsorshipConsent =
  | { enabled: true; consent_version: 1 }
  | { enabled: false };
export type PersonalInferencePreferences = {
  dm_subscription_preference: DmSubscriptionPreference;
  channel_sponsorship_enabled: boolean;
  consent_version: 1 | null;
  inference_sponsorship_supported: boolean;
};
export type PersonalInferencePreferencesUpdate = Omit<
  PersonalInferencePreferences,
  "inference_sponsorship_supported" | "consent_version"
> & { consent_version: 1 | null };
export const CHANNEL_SUBSCRIPTION_CONSENT =
  "Use my eligible subscriptions for Slack channel conversations I start, including teammates’ later replies in those conversations.";
export const SUBSCRIPTION_PREFERENCES_UNAVAILABLE =
  "This Gateway does not support subscription preferences. Ask your administrator to update Gateway. Conversations keep using Gateway defaults.";
export function parsePersonalInferencePreferences(
  value: unknown,
): PersonalInferencePreferences | null {
  if (!value || typeof value !== "object") return null;
  const v = value as Record<string, unknown>;
  if (
    !["prefer_owned_subscription", "gateway_default"].includes(
      String(v.dm_subscription_preference),
    ) ||
    typeof v.channel_sponsorship_enabled !== "boolean" ||
    !(v.consent_version === null || v.consent_version === 1) ||
    (v.channel_sponsorship_enabled && v.consent_version !== 1) ||
    (v.inference_sponsorship_supported !== undefined &&
      typeof v.inference_sponsorship_supported !== "boolean")
  )
    return null;
  return {
    dm_subscription_preference:
      v.dm_subscription_preference as DmSubscriptionPreference,
    channel_sponsorship_enabled: v.channel_sponsorship_enabled,
    consent_version: v.consent_version as 1 | null,
    inference_sponsorship_supported: v.inference_sponsorship_supported === true,
  };
}
