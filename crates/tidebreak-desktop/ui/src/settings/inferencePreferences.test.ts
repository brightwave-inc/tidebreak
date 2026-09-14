import { describe, expect, it } from "vitest";
import { parseCodeConnectPage } from "../code/parsers";
import { parseChannelPreferences } from "./channelPreferences";
import { parsePersonalInferencePreferences } from "./inferencePreferences";
const personal = {
  dm_subscription_preference: "prefer_owned_subscription",
  channel_sponsorship_enabled: false,
  consent_version: null,
};
const channel = {
  harness: null,
  model: null,
  instructions: "",
  respond_automatically: null,
  channel_id: "C1",
  workspace_identity: "T1",
  settings_path: "/settings/channels",
  can_edit: true,
};
const connect = {
  channel_kind: "slack",
  display_name: "Casey",
  workspace_name: "Acme",
  state: "pending",
  csrf: "csrf-token",
  expires_at: "2026-09-14T12:00:00Z",
};
describe("subscription preference wire contracts", () => {
  it("leaves legacy channel PUTs compatible and treats absent capabilities as unsupported", () => {
    expect(parseChannelPreferences(channel)).toEqual({
      ...channel,
      inference_sponsorship_supported: false,
    });
    expect(parseChannelPreferences(channel)).not.toHaveProperty(
      "subscription_preference",
    );
    expect(parseCodeConnectPage(connect)).toEqual({
      ...connect,
      inference_sponsorship_supported: false,
    });
    expect(parsePersonalInferencePreferences(personal)).toEqual({
      ...personal,
      inference_sponsorship_supported: false,
    });
  });
  it("accepts known preferences and explicit versioned consent", () => {
    expect(
      parsePersonalInferencePreferences({
        ...personal,
        channel_sponsorship_enabled: true,
        consent_version: 1,
        inference_sponsorship_supported: true,
      }),
    ).toMatchObject({
      channel_sponsorship_enabled: true,
      consent_version: 1,
      inference_sponsorship_supported: true,
    });
    expect(
      parseChannelPreferences({
        ...channel,
        subscription_preference: "gateway_default",
        inference_sponsorship_supported: true,
      }),
    ).toMatchObject({
      subscription_preference: "gateway_default",
      inference_sponsorship_supported: true,
    });
  });
  it("rejects malformed capabilities and preferences", () => {
    for (const capability of [null, "true", 1]) {
      expect(
        parseChannelPreferences({
          ...channel,
          inference_sponsorship_supported: capability,
        }),
      ).toBeNull();
      expect(
        parseCodeConnectPage({
          ...connect,
          inference_sponsorship_supported: capability,
        }),
      ).toBeNull();
      expect(
        parsePersonalInferencePreferences({
          ...personal,
          inference_sponsorship_supported: capability,
        }),
      ).toBeNull();
    }
    expect(
      parseChannelPreferences({
        ...channel,
        subscription_preference: "any_account",
      }),
    ).toBeNull();
    expect(
      parsePersonalInferencePreferences({
        ...personal,
        dm_subscription_preference: "any_account",
      }),
    ).toBeNull();
    for (const version of [null, 0, 2, "1"]) {
      expect(
        parsePersonalInferencePreferences({
          ...personal,
          channel_sponsorship_enabled: true,
          consent_version: version,
        }),
      ).toBeNull();
    }
  });
});
