import type { Meta, StoryObj } from "@storybook/react-vite";
import type { ApiClient } from "@/api/client";
import { CodeQuickOpen } from "@/code/CodeQuickOpen";
import { codeWorkspaceFilePaths } from "./fixtures";

type QuickOpenScenario = "ready" | "loading" | "empty" | "failure";

function pending<T>(): Promise<T> {
  return new Promise(() => {});
}

function quickOpenClient(scenario: QuickOpenScenario) {
  return {
    listCodeWorkspaceTree: async () => {
      if (scenario === "loading") return pending();
      if (scenario === "failure") {
        throw new Error("The workspace file index is unavailable.");
      }
      return {
        paths: scenario === "empty" ? [] : [...codeWorkspaceFilePaths],
        truncated: false,
      };
    },
  } satisfies Pick<ApiClient, "listCodeWorkspaceTree">;
}

function QuickOpenStory({ scenario }: { scenario: QuickOpenScenario }) {
  return (
    <div className="app-shell h-full min-h-[28rem] w-full bg-page-background">
      <CodeQuickOpen
        client={quickOpenClient(scenario)}
        workspaceId="ws-storybook-audit"
        contentRevision={0}
        openRequest={1}
        onOpenFile={() => {}}
      />
    </div>
  );
}

const meta = {
  title: "Code/Quick open",
  component: QuickOpenStory,
  args: { scenario: "ready" },
  parameters: { layout: "fullscreen" },
  render: (args) => <QuickOpenStory key={args.scenario} {...args} />,
} satisfies Meta<typeof QuickOpenStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Files: Story = {};

export const FilteredResults: Story = {};

export const Loading: Story = { args: { scenario: "loading" } };

export const Empty: Story = { args: { scenario: "empty" } };

export const Failure: Story = {
  args: { scenario: "failure" },
};

export const Compact: Story = {
  globals: { viewport: { value: "compact", isRotated: false } },
};
