import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";
import {
  AgentCursorOverlay,
  type AgentCursorPreview,
} from "../AgentCursorOverlay";
import { ComputerUseActionStatus } from "../ComputerUseActionStatus";
import { ComputerUseIndicatorView } from "../ComputerUseIndicator";
import type { ComputerUseAction } from "../computerUseAction";

const NOW = 1_700_000_000_000;
const action: ComputerUseAction = {
  actionId: "action-1",
  sessionId: "session-1",
  source: "browser",
  action: "click",
  phase: "running",
  executionMode: "background",
  coordinateFrame: "viewport",
  browserId: "browser-1",
  workspaceId: "workspace-1",
  documentEpoch: 3,
  point: { x: 488, y: 237 },
  viewport: { width: 800, height: 420 },
  startedAtMillis: NOW,
  visibleUntilMillis: NOW + 1200,
};
const preview: AgentCursorPreview = {
  source: "browser",
  sessionId: "session-1",
  browserId: "browser-1",
  workspaceId: "workspace-1",
  documentEpoch: 3,
  coordinateFrame: "viewport",
  width: 800,
  height: 420,
};

function PreviewFixture() {
  return (
    <div
      aria-label="Browser preview fixture"
      className="h-full bg-background p-8 text-foreground"
    >
      <div className="flex items-center justify-between border-b border-border-subtle pb-4">
        <span className="text-sm font-semibold">Preview app</span>
        <span className="text-xs text-muted-foreground">Settings</span>
      </div>
      <h2 className="mt-7 text-xl font-semibold">Project settings</h2>
      <p className="mt-2 text-sm text-muted-foreground">
        The agent checks this view while you keep working.
      </p>
      <div className="mt-7 max-w-sm">
        <label htmlFor="project-name" className="text-xs font-medium">
          Project name
        </label>
        <input
          id="project-name"
          className="mt-2 h-control w-full rounded-md border border-border bg-background px-3 text-sm"
          defaultValue="Tidebreak preview"
        />
      </div>
      <button
        type="button"
        className="mt-5 h-control rounded-md bg-foreground px-4 text-sm font-medium text-background"
      >
        Save changes
      </button>
    </div>
  );
}

function BrowserPreview({
  phase = "running",
}: {
  phase?: ComputerUseAction["phase"];
}) {
  const current = { ...action, phase };
  return (
    <div className="flex min-h-dvh items-center justify-center bg-page-background p-4">
      <div className="w-full max-w-3xl overflow-hidden rounded-xl border border-border bg-background">
        <div className="flex items-center gap-2 border-b border-border-subtle px-3 py-2 text-xs">
          <span className="size-2 rounded-full bg-muted-foreground/40" />
          <span className="font-mono text-muted-foreground">
            localhost:5173/settings
          </span>
        </div>
        <ComputerUseActionStatus action={current} now={NOW} />
        <div className="relative aspect-[800/420] min-h-80 overflow-hidden">
          <PreviewFixture />
          <AgentCursorOverlay action={current} preview={preview} now={NOW} />
        </div>
        <div className="border-t border-border-subtle px-3 py-2 text-2xs text-muted-foreground">
          Agent cursor · Your pointer stays free
        </div>
      </div>
    </div>
  );
}

function NativeActivity({
  phase = "running",
  stopped = false,
  longName = false,
  idle = false,
  chrome = false,
  stoppedSessions = 0,
}: {
  phase?: ComputerUseAction["phase"];
  stopped?: boolean;
  longName?: boolean;
  idle?: boolean;
  chrome?: boolean;
  stoppedSessions?: number;
}) {
  const now = Date.now();
  const native: ComputerUseAction = {
    ...action,
    source: chrome ? "chrome" : "native",
    phase,
    action: "type",
    coordinateFrame: "screen",
    bundleId: "dev.tidebreak.fixture",
    windowId: 42,
    startedAtMillis: now,
    visibleUntilMillis: now + 60_000,
  };
  return (
    <div className="grid min-h-dvh place-items-center bg-page-background p-6">
      <div className="max-w-sm text-center">
        <p className="text-md font-medium">Keep working in your editor</p>
        <p className="mt-2 text-sm text-muted-foreground">
          Native activity reports the execution mode. The indicator does not
          move your pointer.
        </p>
      </div>
      <ComputerUseIndicatorView
        snapshot={{
          halted: stopped,
          stoppedSessions,
          active: idle
            ? null
            : {
                bundleId: native.bundleId!,
                appName: longName
                  ? "A preview application with a deliberately long window title"
                  : "Preview fixture",
                lastActivityMillis: now,
                visibleUntilMillis: now + 60_000,
              },
        }}
        action={idle ? null : native}
        onStop={fn(async () => {})}
        onResume={fn(async () => {})}
      />
    </div>
  );
}

const meta = {
  title: "Code/Computer use activity",
  parameters: { layout: "fullscreen" },
} satisfies Meta;
export default meta;
type Story = StoryObj<typeof meta>;
export const BackgroundBrowser: Story = { render: () => <BrowserPreview /> };
export const CompletedBrowser: Story = {
  render: () => <BrowserPreview phase="completed" />,
};
export const BackgroundNative: Story = { render: () => <NativeActivity /> };
export const ForegroundRequired: Story = {
  render: () => <NativeActivity phase="foreground_required" />,
};
export const Failed: Story = {
  render: () => <NativeActivity phase="failed" />,
};
export const Stopped: Story = { render: () => <NativeActivity stopped /> };
export const LongAppName: Story = { render: () => <NativeActivity longName /> };
export const Idle: Story = { render: () => <NativeActivity idle /> };

export const BackgroundChrome: Story = {
  render: () => <NativeActivity chrome />,
};

export const StoppedSession: Story = {
  render: () => <NativeActivity idle stoppedSessions={1} />,
};
export const SeveralStoppedSessions: Story = {
  render: () => <NativeActivity idle stoppedSessions={3} />,
};
export const ActiveWithStoppedSession: Story = {
  render: () => <NativeActivity stoppedSessions={1} />,
};
