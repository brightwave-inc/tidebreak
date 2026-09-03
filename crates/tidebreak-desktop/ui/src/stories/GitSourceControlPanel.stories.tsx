import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";
import type { RuntimeSettings } from "@/api/types";
import { GitSourceControlPanel } from "@/settings/GitSourceControlPanel";
import { storySettings } from "./SettingsStoryHarness";

function settings(
  git: Partial<RuntimeSettings["git_source_control"]> = {},
): RuntimeSettings {
  return {
    ...storySettings,
    git_source_control: {
      ...storySettings.git_source_control,
      ...git,
    },
  };
}

function client(
  initial: RuntimeSettings,
  options: {
    loading?: boolean;
    loadFailure?: boolean;
    saveFailure?: boolean;
  } = {},
) {
  let stored = initial;
  return {
    getSettings: options.loading
      ? () => new Promise<RuntimeSettings>(() => undefined)
      : options.loadFailure
        ? async () => {
            throw new Error("Git settings could not be loaded.");
          }
        : async () => stored,
    putSettings: fn(async (body) => {
      if (options.saveFailure) {
        throw new Error("Git settings could not be saved.");
      }
      const update = body.git_source_control ?? {};
      const mode =
        update.branch_prefix_mode ??
        stored.git_source_control.branch_prefix_mode;
      const custom =
        update.custom_branch_prefix === undefined
          ? stored.git_source_control.custom_branch_prefix
          : update.custom_branch_prefix
            ? `${update.custom_branch_prefix.replace(/\/+$/, "")}/`
            : undefined;
      const effective =
        mode === "none"
          ? ""
          : mode === "custom"
            ? (custom ?? "tidebreak/")
            : (stored.git_source_control.account_prefix ?? "tidebreak/");
      stored = {
        ...stored,
        git_source_control: {
          ...stored.git_source_control,
          ...update,
          branch_prefix_mode: mode,
          custom_branch_prefix: custom,
          effective_branch_prefix: effective,
        },
      };
      return stored;
    }),
  };
}

const accountClient = client(settings());

const meta = {
  title: "Settings/Git & source control",
  component: GitSourceControlPanel,
  parameters: { layout: "fullscreen" },
  args: { client: accountClient as never },
} satisfies Meta<typeof GitSourceControlPanel>;

export default meta;
type Story = StoryObj<typeof meta>;

export const AccountPrefix: Story = {};

export const CustomPrefix: Story = {
  args: {
    client: client(
      settings({
        branch_prefix_mode: "custom",
        custom_branch_prefix: "platform/alex/",
        effective_branch_prefix: "platform/alex/",
      }),
    ) as never,
  },
};

export const NoPrefix: Story = {
  args: {
    client: client(
      settings({
        branch_prefix_mode: "none",
        effective_branch_prefix: "",
      }),
    ) as never,
  },
};

export const AccountUnavailable: Story = {
  args: {
    client: client(
      settings({
        account_prefix: undefined,
        effective_branch_prefix: "tidebreak/",
      }),
    ) as never,
  },
};

export const Loading: Story = {
  args: { client: client(settings(), { loading: true }) as never },
};

export const LoadFailed: Story = {
  args: { client: client(settings(), { loadFailure: true }) as never },
};

export const SaveFailed: Story = {
  args: { client: client(settings(), { saveFailure: true }) as never },
};

export const Narrow: Story = {
  decorators: [
    (Story) => (
      <div className="h-screen w-[390px] border-r border-border">
        <Story />
      </div>
    ),
  ],
};
