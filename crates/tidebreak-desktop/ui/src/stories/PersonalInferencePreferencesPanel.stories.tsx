import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, userEvent, within } from "storybook/test";
import type { ApiClient } from "@/api";
import { PersonalInferencePreferencesPanel } from "@/settings/PersonalInferencePreferencesPanel";
import type {
  PersonalInferencePreferences,
  PersonalInferencePreferencesUpdate,
} from "@/settings/inferencePreferences";
const preferences: PersonalInferencePreferences = {
  dm_subscription_preference: "prefer_owned_subscription",
  channel_sponsorship_enabled: false,
  consent_version: null,
  inference_sponsorship_supported: true,
};
function client(
  overrides: Partial<PersonalInferencePreferences> = {},
): ApiClient {
  const saved = { ...preferences, ...overrides };
  return {
    getPersonalInferencePreferences: async () => saved,
    setPersonalInferencePreferences: async (
      _grant: string,
      update: PersonalInferencePreferencesUpdate,
    ) => ({ ...saved, ...update }),
  } as unknown as ApiClient;
}
const meta = {
  title: "Settings/Personal Slack subscriptions",
  component: PersonalInferencePreferencesPanel,
  args: { client: client(), grantId: "person-grant", onBack: () => {} },
} satisfies Meta<typeof PersonalInferencePreferencesPanel>;
export default meta;
type Story = StoryObj<typeof meta>;
export const ConnectedWithoutChannelConsent: Story = {};
export const ChannelSponsorshipEnabled: Story = {
  args: {
    client: client({ channel_sponsorship_enabled: true, consent_version: 1 }),
  },
};
export const GatewayDefaults: Story = {
  args: { client: client({ dm_subscription_preference: "gateway_default" }) },
};
export const UnsupportedGateway: Story = {
  args: { client: client({ inference_sponsorship_supported: false }) },
};
export const Loading: Story = {
  args: {
    client: {
      getPersonalInferencePreferences: () => new Promise(() => {}),
    } as unknown as ApiClient,
  },
};
export const LoadFailed: Story = {
  args: {
    client: {
      getPersonalInferencePreferences: async () => {
        throw new Error(
          "Subscription settings could not be loaded. Try again.",
        );
      },
    } as unknown as ApiClient,
  },
};
export const SaveFailed: Story = {
  args: {
    client: {
      ...client(),
      setPersonalInferencePreferences: async () => {
        throw new Error("Subscription settings could not be saved. Try again.");
      },
    } as unknown as ApiClient,
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByRole("switch", {
        name: "Use my subscriptions in channels",
      }),
    );
    await expect(await canvas.findByRole("alert")).toHaveTextContent(
      "could not be saved",
    );
    await expect(canvas.getByRole("switch")).not.toBeChecked();
  },
};
