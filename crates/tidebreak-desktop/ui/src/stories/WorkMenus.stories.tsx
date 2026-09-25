import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn, userEvent, within } from "storybook/test";

import type { ModelInfo } from "@/api";
import { ModelMenu } from "@/ModelMenu";
import {
  PermissionModeMenu,
  WORK_PERMISSION_MODE_DESCRIPTIONS,
} from "@/PermissionModeMenu";

/** Menus portal to `document.body`, so stories query the whole page. */
const page = (canvasElement: HTMLElement) =>
  within(canvasElement.ownerDocument.body);

function row(
  provider: ModelInfo["provider"],
  id: string,
  name: string,
): ModelInfo {
  return {
    key: `${provider}::${id}`,
    id,
    display_name: name,
    provider,
    vendor: null,
    verification: "verified",
    recommended: true,
    available: true,
    context_window: 1_000_000,
    max_output_tokens: 128_000,
    input_modalities: ["text", "image"],
    supports_reasoning: true,
    supports_tools: true,
    supports_structured_output: true,
    reasoning_efforts: ["low", "medium", "high"],
    multimodal: true,
  };
}

const models: ModelInfo[] = [
  row("openai", "gpt-5.6-sol", "GPT-5.6 Sol"),
  row("openai", "gpt-6-sol", "GPT-6 Sol"),
  row("anthropic", "claude-opus-5", "Claude Opus 5"),
  row("anthropic", "claude-sonnet-5", "Claude Sonnet 5"),
];

type WorkMenusStoryProps = {
  menu: "permissions" | "model";
  permission: "plan" | "ask" | "auto" | "allow";
};

/** The Work composer's bar, with only the menu the story is about. */
function WorkMenusStory({ menu, permission }: WorkMenusStoryProps) {
  return (
    <div className="flex min-h-[26rem] items-end justify-center p-10">
      <div className="flex items-center gap-2 rounded-xl border border-border bg-background p-2">
        {menu === "model" ? (
          <ModelMenu
            models={models}
            value="anthropic::claude-opus-5"
            defaultKey="openai::gpt-5.6-sol"
            lastUsed
            onSetUpProvider={fn()}
            onChange={fn()}
          />
        ) : (
          <PermissionModeMenu
            scopeKey="story"
            value={permission}
            descriptions={WORK_PERMISSION_MODE_DESCRIPTIONS}
            onChange={fn()}
          />
        )}
      </div>
    </div>
  );
}

const meta = {
  title: "Composer/Work menus",
  component: WorkMenusStory,
  args: { menu: "permissions", permission: "ask" },
  parameters: { layout: "fullscreen" },
} satisfies Meta<typeof WorkMenusStory>;

export default meta;
type Story = StoryObj<typeof meta>;

/**
 * Each mode says what it does. The button names the mode in force, and its
 * tooltip says what that mode does.
 */
export const PermissionModes: Story = {
  play: async ({ canvasElement }) => {
    await userEvent.click(
      page(canvasElement).getByRole("button", { name: "Permissions: Ask" }),
    );
  },
};

/** The trigger's tooltip, naming the posture in force. */
export const PermissionPosture: Story = {
  args: { permission: "auto" },
  play: async ({ canvasElement }) => {
    await userEvent.hover(
      page(canvasElement).getByRole("button", { name: "Permissions: Auto" }),
    );
  },
};

/**
 * A new conversation starts on the model picked last. When that is not the
 * default Settings names, the menu says so and marks the default.
 */
export const LastUsedAndDefault: Story = {
  args: { menu: "model" },
  play: async ({ canvasElement }) => {
    await userEvent.click(
      page(canvasElement).getByRole("button", { name: "Model: Claude Opus 5" }),
    );
  },
};

/** The permission menu at the 420 px compact pane. */
export const PermissionModesCompact: Story = {
  ...PermissionModes,
  globals: { viewport: { value: "compact", isRotated: false } },
};
