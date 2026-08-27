import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, fn, userEvent, waitFor, within } from "storybook/test";

import { SidebarAccountMenuPanel } from "@/sidebar/SidebarAccountMenu";
import { railAccountIdentity } from "@/sidebar/railAccountIdentity";
import { gatewaySignedIn } from "./fixtures";

const meta = {
  title: "Navigation/Account menu",
  component: SidebarAccountMenuPanel,
  args: {
    identity: railAccountIdentity({ gateway: null }),
    themeMode: "system",
    onSettings: fn(),
    onThemeMode: fn(),
  },
  decorators: [
    (Story) => (
      <div className="bg-page-background flex h-[28rem] w-[264px] flex-col justify-end rounded-lg border border-border-subtle p-2">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof SidebarAccountMenuPanel>;

export default meta;
type Story = StoryObj<typeof meta>;

/** No gateway or GitHub session: an empty chip, settings still one click away. */
export const Empty: Story = {};

export const ModelGateway: Story = {
  args: {
    identity: railAccountIdentity({ gateway: gatewaySignedIn }),
  },
};

export const GitHub: Story = {
  args: {
    identity: railAccountIdentity({
      gateway: null,
      githubLogin: "github",
    }),
  },
};

export const GatewayAndGitHub: Story = {
  args: {
    identity: railAccountIdentity({
      gateway: gatewaySignedIn,
      githubLogin: "github",
    }),
  },
};

export const Open: Story = {
  args: {
    identity: railAccountIdentity({ gateway: gatewaySignedIn }),
    defaultOpen: true,
  },
};

export const OpenTheme: Story = {
  args: {
    identity: railAccountIdentity({ gateway: gatewaySignedIn }),
    themeMode: "dark",
  },
  play: async ({ canvasElement }) => {
    const body = within(canvasElement.ownerDocument.body);
    await userEvent.click(
      await body.findByRole("button", { name: "Account menu" }),
    );
    await userEvent.click(await body.findByRole("menuitem", { name: "Theme" }));
    await waitFor(() =>
      expect(body.getByRole("menuitem", { name: "Dark" })).toBeVisible(),
    );
  },
};
