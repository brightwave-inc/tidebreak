// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import type { ApiClient } from "@/api";
import { PersonalInferencePreferencesPanel } from "./PersonalInferencePreferencesPanel";
import {
  CHANNEL_SUBSCRIPTION_CONSENT,
  type PersonalInferencePreferences,
  type PersonalInferencePreferencesUpdate,
} from "./inferencePreferences";
const preferences: PersonalInferencePreferences = {
  dm_subscription_preference: "prefer_owned_subscription",
  channel_sponsorship_enabled: false,
  consent_version: null,
  inference_sponsorship_supported: true,
};
function client(value = preferences) {
  return {
    getPersonalInferencePreferences: vi.fn(async () => value),
    setPersonalInferencePreferences: vi.fn(
      async (_grant: string, update: PersonalInferencePreferencesUpdate) => ({
        ...value,
        ...update,
      }),
    ),
  };
}
afterEach(cleanup);
it("records explicit versioned consent and revokes it without reconnecting", async () => {
  const api = client();
  render(
    <PersonalInferencePreferencesPanel
      client={api as unknown as ApiClient}
      grantId="mine"
      onBack={() => {}}
    />,
  );
  const user = userEvent.setup();
  const toggle = await screen.findByRole("switch", {
    name: "Use my subscriptions in channels",
  });
  expect(screen.getByText(CHANNEL_SUBSCRIPTION_CONSENT)).toBeVisible();
  expect(toggle).not.toBeChecked();
  await user.click(toggle);
  await waitFor(() =>
    expect(api.setPersonalInferencePreferences).toHaveBeenLastCalledWith(
      "mine",
      {
        dm_subscription_preference: "prefer_owned_subscription",
        channel_sponsorship_enabled: true,
        consent_version: 1,
      },
    ),
  );
  await waitFor(() => expect(toggle).toBeEnabled());
  await user.click(toggle);
  await waitFor(() =>
    expect(api.setPersonalInferencePreferences).toHaveBeenLastCalledWith(
      "mine",
      {
        dm_subscription_preference: "prefer_owned_subscription",
        channel_sponsorship_enabled: false,
        consent_version: null,
      },
    ),
  );
});
it("changes the DM default without granting channel consent", async () => {
  const api = client();
  render(
    <PersonalInferencePreferencesPanel
      client={api as unknown as ApiClient}
      grantId="mine"
      onBack={() => {}}
    />,
  );
  const user = userEvent.setup();
  await user.click(
    await screen.findByRole("combobox", { name: "DM subscription preference" }),
  );
  await user.click(
    await screen.findByRole("option", { name: "Use Gateway defaults" }),
  );
  await waitFor(() =>
    expect(api.setPersonalInferencePreferences).toHaveBeenCalledWith("mine", {
      dm_subscription_preference: "gateway_default",
      channel_sponsorship_enabled: false,
      consent_version: null,
    }),
  );
});
it("does not show unsupported preferences as available or permit writes", async () => {
  const api = client({
    ...preferences,
    inference_sponsorship_supported: false,
  });
  render(
    <PersonalInferencePreferencesPanel
      client={api as unknown as ApiClient}
      grantId="mine"
      onBack={() => {}}
    />,
  );
  expect(await screen.findByRole("switch")).toBeDisabled();
  expect(screen.getByRole("combobox")).toBeDisabled();
  expect(
    screen.getByText(/does not support subscription preferences/),
  ).toBeVisible();
  expect(api.setPersonalInferencePreferences).not.toHaveBeenCalled();
});
it("retains the saved state after a rejected consent update", async () => {
  const api = client();
  api.setPersonalInferencePreferences.mockRejectedValue(
    new Error("Your Gateway connection needs to be renewed."),
  );
  render(
    <PersonalInferencePreferencesPanel
      client={api as unknown as ApiClient}
      grantId="mine"
      onBack={() => {}}
    />,
  );
  await userEvent.setup().click(await screen.findByRole("switch"));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "needs to be renewed",
  );
  expect(screen.getByRole("switch")).not.toBeChecked();
  expect(screen.getByRole("switch")).toBeEnabled();
});
it("ignores a save response after opening another personal connection", async () => {
  const api = client();
  let finish: (value: PersonalInferencePreferences) => void = () => {};
  api.setPersonalInferencePreferences.mockImplementation(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  const view = render(
    <PersonalInferencePreferencesPanel
      client={api as unknown as ApiClient}
      grantId="first"
      onBack={() => {}}
    />,
  );
  await userEvent.setup().click(await screen.findByRole("switch"));
  view.rerender(
    <PersonalInferencePreferencesPanel
      client={api as unknown as ApiClient}
      grantId="second"
      onBack={() => {}}
    />,
  );
  await screen.findByRole("switch");
  finish({
    ...preferences,
    channel_sponsorship_enabled: true,
    consent_version: 1,
  });
  await waitFor(() => expect(screen.getByRole("switch")).not.toBeChecked());
});
