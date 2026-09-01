import { useState } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, fn, userEvent, waitFor, within } from "storybook/test";

import {
  BrowserAgentControlRow,
  BrowserNoticeRow,
  BrowserToolbar,
} from "@/code/browser/BrowserToolbar";
import { BrowserViewportControl } from "@/code/browser/BrowserViewportControl";
import { BrowserFallback } from "@/code/browser/CodeBrowserTab";
import type { BrowserViewport } from "@/code/browser/browserViewport";
import type {
  BrowserAgentAccess,
  BrowserController,
  BrowserHostSnapshot,
} from "@/code/browser/browserHost";
import type { BrowserSession } from "@/code/browser/browserSession";
import { cn } from "@/lib/utils";

type BrowserScenario =
  | "empty"
  | "loading"
  | "unshared"
  | "shared"
  | "paused"
  | "agent"
  | "takeover"
  | "slow"
  | "failure"
  | "popup"
  | "download"
  | "download-failed"
  | "download-saved"
  | "viewport-fit"
  | "viewport-desktop"
  | "viewport-tablet"
  | "viewport-mobile"
  | "viewport-custom"
  | "same-document"
  | "inspect-enable-failure"
  | "inspect-remove-failure"
  | "inspect-off"
  | "inspect-on"
  | "unsupported-frame"
  | "profile-reset-confirmation"
  | "profile-resetting"
  | "profile-reset-reconstructing"
  | "profile-reset-failure";

const inspectEngine: NonNullable<BrowserHostSnapshot["engine"]> = {
  name: "wk_webview",
  capabilities: {
    lifecycle: true,
    persistentProfile: true,
    semanticSnapshot: true,
    semanticActions: false,
    screenshot: false,
    crossOriginFrames: false,
    profileReset: false,
  },
};
const resetEngine: NonNullable<BrowserHostSnapshot["engine"]> = {
  ...inspectEngine,
  capabilities: {
    ...inspectEngine.capabilities,
    profileReset: true,
  },
};

const activeAgent: BrowserController = {
  kind: "agent",
  label: "Code agent",
  action: "Inspecting the deployment preview",
  halted: false,
  takeoverRequired: false,
};

const takeoverAgent: BrowserController = {
  kind: "agent",
  label: "Code agent",
  action: "Enter the one-time verification code to continue",
  halted: false,
  takeoverRequired: true,
};

const unsharedAccess: BrowserAgentAccess = {
  shared: false,
  paused: false,
  halted: false,
  origin: "http://localhost:4173",
  canObserve: false,
  canControl: false,
  canTransferFiles: false,
};

const localSharedAccess: BrowserAgentAccess = {
  shared: true,
  paused: false,
  halted: false,
  origin: "http://localhost:4173",
  scope: "loopback_workspace",
  canObserve: true,
  canControl: true,
  canTransferFiles: false,
};

const pausedAccess: BrowserAgentAccess = {
  shared: false,
  paused: true,
  halted: true,
  origin: "https://accounts.example.org",
  canObserve: false,
  canControl: false,
  canTransferFiles: false,
};

const now = Date.parse("2026-08-20T16:20:00.000Z");

function browserSession(
  loadState: BrowserSession["loadState"],
): BrowserSession {
  const hasUrl = loadState !== "idle";
  const url = hasUrl ? "http://localhost:4173/review/browser" : null;
  return {
    version: 1,
    id: "browser-story",
    workspaceId: "workspace-story",
    url,
    address: url ? "localhost:4173/review/browser" : "",
    title: hasUrl ? "Browser review — Tidebreak" : "Browser",
    loadState,
    error:
      loadState === "failed" ? "The local preview stopped responding." : null,
    notice: null,
    inspectEnabled: false,
    history: url
      ? [
          {
            url: "https://github.com/brightwave-inc/tidebreak/pull/2335",
            title: "Agent browser epic",
          },
          { url, title: "Browser review — Tidebreak" },
        ]
      : [],
    historyIndex: url ? 1 : -1,
    updatedAt: now,
  };
}

function BrowserStory({
  scenario,
  compact = false,
  viewport,
}: {
  scenario: BrowserScenario;
  compact?: boolean;
  viewport?: BrowserViewport;
}) {
  const loadState =
    scenario === "empty"
      ? "idle"
      : scenario === "loading" || scenario === "slow"
        ? "loading"
        : scenario === "failure"
          ? "failed"
          : "ready";
  const baseSession = browserSession(loadState);
  const sameDocumentUrl =
    "http://localhost:4173/review/browser?view=replaced#summary";
  const session =
    scenario === "same-document"
      ? {
          ...baseSession,
          url: sameDocumentUrl,
          address: "localhost:4173/review/browser?view=replaced#summary",
          title: "Browser review — summary",
          history: [
            ...baseSession.history,
            {
              url: sameDocumentUrl,
              title: "Browser review — summary",
            },
          ],
          historyIndex: baseSession.history.length,
        }
      : baseSession;
  const [address, setAddress] = useState(session.address);
  const [viewportState, setViewportState] = useState<BrowserViewport>(
    viewport ?? { preset: "fit", customWidth: 1024 },
  );
  const [profileResetStarted, setProfileResetStarted] = useState(false);
  const inspectEnabled =
    scenario === "inspect-on" ||
    scenario === "inspect-remove-failure" ||
    scenario === "unsupported-frame";
  const inspectFailure =
    scenario === "inspect-enable-failure"
      ? "Could not show inspect highlights"
      : scenario === "inspect-remove-failure"
        ? "Could not hide inspect highlights"
        : null;
  const controller =
    scenario === "agent"
      ? activeAgent
      : scenario === "takeover"
        ? takeoverAgent
        : undefined;
  const agentAccess =
    scenario === "empty" || scenario === "failure"
      ? undefined
      : scenario === "shared" || scenario === "agent" || scenario === "takeover"
        ? localSharedAccess
        : scenario === "paused"
          ? pausedAccess
          : unsharedAccess;
  const profileResetScenario =
    scenario === "profile-reset-confirmation" ||
    scenario === "profile-resetting" ||
    scenario === "profile-reset-reconstructing" ||
    scenario === "profile-reset-failure";
  const profileResetPhase = profileResetStarted
    ? scenario === "profile-resetting"
      ? "deleting"
      : scenario === "profile-reset-reconstructing"
        ? "reconstructing"
        : null
    : null;
  const onResetProfile =
    scenario === "profile-resetting" ||
    scenario === "profile-reset-reconstructing"
      ? () => {
          setProfileResetStarted(true);
          return new Promise<void>(() => {});
        }
      : scenario === "profile-reset-failure"
        ? () =>
            Promise.reject(
              new Error("WebKit could not remove the managed profile data"),
            )
        : profileResetScenario
          ? () => Promise.resolve()
          : undefined;

  return (
    <div className="grid min-h-dvh place-items-center bg-muted/45 p-4 sm:p-8">
      <div
        className={cn(
          "flex h-[min(760px,calc(100dvh-4rem))] min-h-[520px] w-full flex-col overflow-hidden rounded-xl border border-border-subtle bg-background shadow-[0_22px_70px_color-mix(in_oklch,var(--foreground)_14%,transparent)]",
          compact ? "max-w-[470px]" : "max-w-[1120px]",
        )}
      >
        <BrowserToolbar
          session={session}
          address={address}
          addressError={null}
          canGoBack={session.historyIndex > 0}
          canGoForward={false}
          controller={controller}
          agentAccess={agentAccess}
          engine={profileResetScenario ? resetEngine : inspectEngine}
          onAddressChange={setAddress}
          onNavigate={fn()}
          onBack={fn()}
          onForward={fn()}
          onReload={fn()}
          onStop={fn()}
          onStopAgent={fn()}
          onTakeOver={fn()}
          onShareAgent={fn()}
          onRevokeAgent={fn()}
          onSelectHistory={fn()}
          onOpenExternal={fn()}
          onResetProfile={onResetProfile}
          profileResetPhase={profileResetPhase}
          onOverlayOpenChange={fn()}
          onAgentAccessOpenChange={fn()}
          onToggleInspect={fn()}
          inspectEnabled={inspectEnabled}
          viewportControl={
            <BrowserViewportControl
              viewport={viewportState}
              renderedWidth={
                viewportState.preset === "fit"
                  ? null
                  : viewportState.preset === "custom"
                    ? viewportState.customWidth
                    : viewportState.preset === "desktop"
                      ? 1440
                      : viewportState.preset === "tablet"
                        ? 768
                        : 390
              }
              onViewportChange={setViewportState}
              disabled={!session.url}
            />
          }
        />

        {scenario === "slow" && (
          <BrowserNoticeRow
            tone="info"
            message="This page is taking longer than expected"
            actionLabel="Open externally"
            onAction={fn()}
            onDismiss={fn()}
          />
        )}
        {scenario === "popup" && (
          <BrowserNoticeRow
            message="This page tried to open a new window"
            actionLabel="Open here"
            onAction={fn()}
            onDismiss={fn()}
          />
        )}
        {scenario === "download" && (
          <BrowserNoticeRow
            message="Downloads stay blocked until Tidebreak has a bounded destination"
            actionLabel="Open externally"
            onAction={fn()}
            onDismiss={fn()}
          />
        )}
        {scenario === "download-failed" && (
          <BrowserNoticeRow
            message="report.md: The downloaded text file is not valid UTF-8"
            actionLabel="Open externally"
            onAction={fn()}
            onDismiss={fn()}
          />
        )}
        {scenario === "download-saved" && (
          <BrowserNoticeRow
            tone="info"
            message="Saved quarterly-report.pdf to Outputs"
            onDismiss={fn()}
          />
        )}
        {inspectFailure && (
          <BrowserNoticeRow
            tone="critical"
            message={inspectFailure}
            onDismiss={fn()}
          />
        )}

        <div className="relative min-h-0 flex-1 overflow-hidden">
          {viewportState.preset !== "fit" &&
            scenario !== "empty" &&
            scenario !== "failure" && (
              <div aria-hidden className="absolute inset-0 bg-muted/30" />
            )}
          <div
            className={cn(
              "relative h-full",
              viewportState.preset === "fit" ? "w-full" : "mx-auto",
            )}
            style={
              viewportState.preset === "fit"
                ? undefined
                : {
                    width:
                      viewportState.preset === "custom"
                        ? `${Math.min(viewportState.customWidth, 1120)}px`
                        : viewportState.preset === "desktop"
                          ? "1120px"
                          : viewportState.preset === "tablet"
                            ? "768px"
                            : "390px",
                    maxWidth: "100%",
                  }
            }
          >
            {scenario === "empty" ? (
              <BrowserFallback error={null} hasUrl={false} />
            ) : scenario === "failure" ? (
              <BrowserFallback
                error="The local preview stopped responding. Restart the dev server, then try again."
                hasUrl
                onRetry={fn()}
                onOpenExternal={fn()}
              />
            ) : scenario === "loading" || scenario === "slow" ? (
              <LoadingPage />
            ) : (
              <DeveloperPage
                compact={compact}
                unsupportedFrame={scenario === "unsupported-frame"}
              />
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

function NarrowToolbarStory({
  width,
  access,
  resetProfile = false,
}: {
  width: 320 | 390;
  access: BrowserAgentAccess;
  resetProfile?: boolean;
}) {
  const session = browserSession("ready");
  const [address, setAddress] = useState(session.address);
  const [viewport, setViewport] = useState<BrowserViewport>({
    preset: "mobile",
    customWidth: 1024,
  });

  return (
    <div className="grid min-h-dvh place-items-center bg-muted/45 p-4">
      <div
        className="flex h-[min(760px,calc(100dvh-4rem))] w-full flex-col overflow-hidden rounded-xl border border-border-subtle bg-background shadow-lg"
        style={{ maxWidth: width }}
      >
        <BrowserToolbar
          session={session}
          address={address}
          addressError={null}
          canGoBack={session.historyIndex > 0}
          canGoForward={false}
          controller={undefined}
          agentAccess={access}
          engine={resetProfile ? resetEngine : inspectEngine}
          onAddressChange={setAddress}
          onNavigate={fn()}
          onBack={fn()}
          onForward={fn()}
          onReload={fn()}
          onStop={fn()}
          onStopAgent={fn()}
          onTakeOver={fn()}
          onShareAgent={fn()}
          onRevokeAgent={fn()}
          onSelectHistory={fn()}
          onResetProfile={resetProfile ? () => Promise.resolve() : undefined}
          onOpenExternal={fn()}
          onOverlayOpenChange={fn()}
          onAgentAccessOpenChange={fn()}
          onToggleInspect={fn()}
          inspectEnabled={false}
          viewportControl={
            <BrowserViewportControl
              viewport={viewport}
              renderedWidth={390}
              onViewportChange={setViewport}
            />
          }
        />
        <div className="relative min-h-0 flex-1 overflow-hidden">
          <div className="absolute inset-0 bg-muted/30" aria-hidden />
          <div
            className="relative mx-auto h-full"
            style={{ width: 390, maxWidth: "100%" }}
          >
            <DeveloperPage compact />
          </div>
        </div>
      </div>
    </div>
  );
}

function DeveloperPage({
  compact,
  unsupportedFrame = false,
}: {
  compact: boolean;
  unsupportedFrame?: boolean;
}) {
  return (
    <div className="h-full overflow-auto bg-[#f1eee7] text-[#20211f]">
      <header className="flex h-14 items-center justify-between border-b border-black/10 px-5 sm:px-8">
        <span className="text-sm font-semibold tracking-[-0.02em]">
          Tidebreak / browser lab
        </span>
        <nav
          className={cn(
            "hidden items-center gap-5 text-xs text-black/55 sm:flex",
            compact && "sm:hidden",
          )}
        >
          <span>Fixture</span>
          <span>Contracts</span>
          <span>Runs</span>
        </nav>
        <span className="font-mono text-2xs text-black/45">localhost:4173</span>
      </header>
      <main
        className={cn(
          "mx-auto max-w-5xl px-5 py-10 sm:px-8 sm:py-16",
          compact && "py-10",
        )}
      >
        <p className="font-mono text-2xs tracking-[0.16em] text-[#7b5e36] uppercase">
          deterministic browser fixture
        </p>
        <h1 className="mt-3 max-w-3xl text-[clamp(2rem,6vw,4.75rem)] font-semibold leading-[0.96] tracking-[-0.055em] text-balance">
          One page for people and agents.
        </h1>
        <p className="mt-5 max-w-xl text-sm leading-6 text-black/58 sm:text-base sm:leading-7">
          Inspect the live document, act on fresh semantic targets, and hand
          control back without losing the browser state you were both using.
        </p>
        <div className="mt-8 flex flex-wrap items-center gap-3">
          <button className="rounded-lg bg-[#20211f] px-4 py-2.5 text-xs font-medium text-white shadow-sm">
            Run checks
          </button>
          <button className="rounded-lg border border-black/15 bg-white/45 px-4 py-2.5 text-xs font-medium">
            Replace target
          </button>
        </div>
        {unsupportedFrame && (
          <section className="relative mt-8 min-h-44 overflow-hidden rounded-xl border border-black/10 bg-[#faf7f0]">
            <div className="grid h-full min-h-44 grid-cols-[1fr_auto] gap-8 p-6 opacity-45 sm:p-8">
              <div>
                <p className="font-mono text-2xs text-black/45">
                  third-party preview
                </p>
                <p className="mt-5 text-lg font-semibold">
                  Hosted payment summary
                </p>
                <p className="mt-2 max-w-md text-sm leading-6 text-black/55">
                  This content comes from another origin and stays outside the
                  semantic snapshot.
                </p>
              </div>
              <div className="hidden w-32 rounded-lg bg-black/8 sm:block" />
            </div>
            <div className="absolute inset-2 grid place-items-center rounded-lg border border-warning-border bg-warning-background/92 px-5 text-center text-sm font-medium text-warning-foreground backdrop-blur-[2px]">
              Uninspectable frame · human takeover
            </div>
          </section>
        )}
        {!compact && (
          <div
            className={cn(
              "grid gap-px overflow-hidden rounded-xl border border-black/10 bg-black/10 sm:grid-cols-[1.25fr_0.75fr]",
              unsupportedFrame ? "mt-6" : "mt-16",
            )}
          >
            <section className="bg-[#f8f5ee] p-6 sm:p-8">
              <p className="font-mono text-2xs text-black/45">
                latest snapshot
              </p>
              <p className="mt-8 text-3xl font-medium tracking-[-0.04em]">
                42 semantic nodes
              </p>
              <p className="mt-2 max-w-sm text-sm leading-6 text-black/55">
                Visible text and interactive controls, bounded and explicitly
                treated as untrusted page data.
              </p>
            </section>
            <section className="bg-[#ded8cc] p-6 sm:p-8">
              <p className="font-mono text-2xs text-black/45">document epoch</p>
              <p className="mt-8 font-mono text-3xl tracking-[-0.04em]">008</p>
              <p className="mt-2 text-sm leading-6 text-black/55">
                Every navigation retires the old refs.
              </p>
            </section>
          </div>
        )}
      </main>
    </div>
  );
}

function SplitPaneBrowserStory() {
  const session = browserSession("ready");
  const [address, setAddress] = useState(session.address);
  const [viewport, setViewport] = useState<BrowserViewport>({
    preset: "fit",
    customWidth: 1024,
  });

  return (
    <div className="grid min-h-dvh place-items-center overflow-auto bg-page-background p-5">
      <div className="grid h-[min(760px,calc(100dvh-2.5rem))] min-h-[560px] w-full min-w-[760px] max-w-[1400px] grid-cols-[minmax(320px,0.9fr)_minmax(420px,1.1fr)] overflow-hidden rounded-xl border border-border bg-background shadow-lg">
        <section
          aria-label="Source editor"
          className="flex min-w-0 flex-col border-r border-border"
        >
          <div className="flex h-10 shrink-0 items-center border-b border-border-subtle px-3">
            <span className="font-mono text-xs text-muted-foreground">
              browser_semantics.rs
            </span>
          </div>
          <div className="min-h-0 flex-1 overflow-hidden bg-muted/25 p-5 font-mono text-xs leading-6 text-muted-foreground">
            <p className="text-foreground">pub enum BrowserFrameStatus {"{"}</p>
            <p className="pl-5">SameOrigin,</p>
            <p className="pl-5 text-warning-foreground">UnsupportedFrame,</p>
            <p className="text-foreground">{"}"}</p>
            <p className="mt-7 text-foreground">
              // The browser stays useful beside the source.
            </p>
            <p className="mt-2">
              // Its toolbar and control row keep their full actions.
            </p>
          </div>
        </section>

        <section
          aria-label="Browser split pane"
          className="flex min-w-0 flex-col bg-background"
        >
          <BrowserToolbar
            session={session}
            address={address}
            addressError={null}
            canGoBack
            canGoForward={false}
            controller={activeAgent}
            agentAccess={localSharedAccess}
            engine={inspectEngine}
            onAddressChange={setAddress}
            onNavigate={fn()}
            onBack={fn()}
            onForward={fn()}
            onReload={fn()}
            onStop={fn()}
            onStopAgent={fn()}
            onTakeOver={fn()}
            onShareAgent={fn()}
            onRevokeAgent={fn()}
            onSelectHistory={fn()}
            onOpenExternal={fn()}
            onOverlayOpenChange={fn()}
            onAgentAccessOpenChange={fn()}
            onToggleInspect={fn()}
            inspectEnabled={false}
            viewportControl={
              <BrowserViewportControl
                viewport={viewport}
                renderedWidth={null}
                onViewportChange={setViewport}
              />
            }
          />
          <div className="min-h-0 flex-1 overflow-hidden">
            <DeveloperPage compact />
          </div>
        </section>
      </div>
    </div>
  );
}

function LoadingPage() {
  return (
    <div className="h-full bg-[#f1eee7] p-8 sm:p-14" aria-label="Page loading">
      <div className="h-3 w-28 animate-pulse rounded bg-black/10 motion-reduce:animate-none" />
      <div className="mt-12 h-12 w-[min(80%,38rem)] animate-pulse rounded-lg bg-black/10 motion-reduce:animate-none" />
      <div className="mt-3 h-12 w-[min(64%,30rem)] animate-pulse rounded-lg bg-black/10 motion-reduce:animate-none" />
      <div className="mt-7 h-3 w-[min(72%,32rem)] animate-pulse rounded bg-black/8 motion-reduce:animate-none" />
      <div className="mt-2 h-3 w-[min(56%,25rem)] animate-pulse rounded bg-black/8 motion-reduce:animate-none" />
      <div className="mt-12 grid gap-3 sm:grid-cols-2">
        <div className="h-48 animate-pulse rounded-xl bg-black/8 motion-reduce:animate-none" />
        <div className="h-48 animate-pulse rounded-xl bg-black/8 motion-reduce:animate-none" />
      </div>
    </div>
  );
}

function ControlRows() {
  return (
    <div className="grid min-h-dvh place-items-center bg-muted/45 p-8">
      <div className="w-full max-w-4xl overflow-hidden rounded-xl border border-border-subtle bg-background shadow-lg">
        <BrowserAgentControlRow
          controller={
            activeAgent as Extract<BrowserController, { kind: "agent" }>
          }
          onStop={fn()}
          onTakeOver={fn()}
        />
        <BrowserAgentControlRow
          controller={
            {
              ...activeAgent,
              halted: true,
              action: undefined,
            } as Extract<BrowserController, { kind: "agent" }>
          }
          onTakeOver={fn()}
        />
        <BrowserAgentControlRow
          controller={
            takeoverAgent as Extract<BrowserController, { kind: "agent" }>
          }
          onStop={fn()}
          onTakeOver={fn()}
        />
      </div>
    </div>
  );
}

const meta = {
  title: "Code/Browser",
  component: BrowserStory,
  parameters: { layout: "fullscreen" },
} satisfies Meta<typeof BrowserStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Empty: Story = {
  args: { scenario: "empty" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      canvas.getByRole("textbox", { name: "Address or search" }),
    ).toHaveFocus();
    await expect(
      canvas.getByText("Bring the live work into the workspace"),
    ).toBeVisible();
  },
};

export const Loading: Story = { args: { scenario: "loading" } };

export const ReadyLocalUnshared: Story = {
  args: { scenario: "unshared" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      canvas.getByRole("button", { name: "Share with agent" }),
    ).toBeVisible();
  },
};

export const SharedLocalSites: Story = {
  args: { scenario: "shared" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText("Local sites shared")).toBeVisible();
    await expect(
      canvas.getByRole("button", { name: "Stop sharing" }),
    ).toBeVisible();
  },
};

export const AgentPausedAtNewOrigin: Story = {
  args: { scenario: "paused" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText("Agent paused")).toBeVisible();
    await expect(
      canvas.getByRole("button", { name: "Review & resume" }),
    ).toBeVisible();
  },
};

export const AgentControlled: Story = {
  args: { scenario: "agent" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      canvas.getByText("Code agent is using this tab"),
    ).toBeVisible();
    await expect(canvas.getByRole("button", { name: "Stop" })).toBeVisible();
    await expect(
      canvas.getByRole("button", { name: "Take over" }),
    ).toBeVisible();
  },
};

export const ManagedProfileResetConfirmation: Story = {
  args: { scenario: "profile-reset-confirmation" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const body = within(canvasElement.ownerDocument.body);
    await userEvent.click(
      canvas.getByRole("button", { name: "Browser options" }),
    );
    await userEvent.click(
      await body.findByRole("menuitem", {
        name: "Reset development profile",
      }),
    );
    const dialog = await body.findByRole("alertdialog");
    await waitFor(() => expect(dialog).toBeVisible());
  },
};

export const ManagedProfileResetting: Story = {
  args: { scenario: "profile-resetting" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const body = within(canvasElement.ownerDocument.body);
    await userEvent.click(
      canvas.getByRole("button", { name: "Browser options" }),
    );
    await userEvent.click(
      await body.findByRole("menuitem", {
        name: "Reset development profile",
      }),
    );
    await userEvent.click(
      await body.findByRole("button", {
        name: "Reset development profile",
      }),
    );
    await expect(
      canvas.getByText(
        "Resetting the Tidebreak development profile… deleting managed cookies, site data, and cache.",
      ),
    ).toBeVisible();
  },
};

export const ManagedProfileReconstructing: Story = {
  args: { scenario: "profile-reset-reconstructing" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const body = within(canvasElement.ownerDocument.body);
    await userEvent.click(
      canvas.getByRole("button", { name: "Browser options" }),
    );
    await userEvent.click(
      await body.findByRole("menuitem", {
        name: "Reset development profile",
      }),
    );
    await userEvent.click(
      await body.findByRole("button", {
        name: "Reset development profile",
      }),
    );
    await expect(
      canvas.getByText(
        "Resetting the Tidebreak development profile… reopening stored browser pages.",
      ),
    ).toBeVisible();
  },
};

export const ManagedProfileResetFailure: Story = {
  args: { scenario: "profile-reset-failure" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const body = within(canvasElement.ownerDocument.body);
    await userEvent.click(
      canvas.getByRole("button", { name: "Browser options" }),
    );
    await userEvent.click(
      await body.findByRole("menuitem", {
        name: "Reset development profile",
      }),
    );
    await userEvent.click(
      await body.findByRole("button", {
        name: "Reset development profile",
      }),
    );
    await expect(
      canvas.getByText("WebKit could not remove the managed profile data"),
    ).toBeVisible();
  },
};

export const HumanTakeoverRequired: Story = { args: { scenario: "takeover" } };

export const SlowPage: Story = { args: { scenario: "slow" } };

export const Failure: Story = {
  args: { scenario: "failure" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByRole("button", { name: "Reload" })).toBeVisible();
    await expect(
      canvas.getByRole("button", { name: "Try again" }),
    ).toBeVisible();
  },
};

export const PopupBlocked: Story = { args: { scenario: "popup" } };

export const DownloadBlocked: Story = { args: { scenario: "download" } };

export const DownloadFailed: Story = {
  args: { scenario: "download-failed" },
};

export const DownloadSaved: Story = { args: { scenario: "download-saved" } };

export const Compact: Story = { args: { scenario: "agent", compact: true } };

export const ViewportFit: Story = {
  args: {
    scenario: "viewport-fit",
    viewport: { preset: "fit", customWidth: 1024 },
  },
};

export const ViewportDesktop: Story = {
  args: {
    scenario: "viewport-desktop",
    viewport: { preset: "desktop", customWidth: 1024 },
  },
};

export const ViewportTablet: Story = {
  args: {
    scenario: "viewport-tablet",
    viewport: { preset: "tablet", customWidth: 1024 },
  },
};

export const ViewportMobile: Story = {
  args: {
    scenario: "viewport-mobile",
    viewport: { preset: "mobile", customWidth: 1024 },
  },
};

export const ViewportCustom: Story = {
  args: {
    scenario: "viewport-custom",
    viewport: { preset: "custom", customWidth: 480 },
  },
};

export const ViewportCompact: Story = {
  args: {
    scenario: "viewport-tablet",
    compact: true,
    viewport: { preset: "tablet", customWidth: 1024 },
  },
};

export const ToolbarNarrow320: Story = {
  args: { scenario: "unshared" },
  render: () => <NarrowToolbarStory width={320} access={unsharedAccess} />,
};

export const ToolbarNarrow390: Story = {
  args: { scenario: "unshared" },
  render: () => <NarrowToolbarStory width={390} access={unsharedAccess} />,
};

export const ToolbarNarrow320Shared: Story = {
  args: { scenario: "shared" },
  render: () => <NarrowToolbarStory width={320} access={localSharedAccess} />,
};

export const ToolbarNarrow390Shared: Story = {
  args: { scenario: "shared" },
  render: () => <NarrowToolbarStory width={390} access={localSharedAccess} />,
};

export const ToolbarNarrow320Paused: Story = {
  args: { scenario: "paused" },
  render: () => (
    <NarrowToolbarStory
      width={320}
      access={{ ...pausedAccess, shared: true, scope: "origin" }}
    />
  ),
};

export const ToolbarNarrow390Paused: Story = {
  args: { scenario: "paused" },
  render: () => (
    <NarrowToolbarStory
      width={390}
      access={{ ...pausedAccess, shared: true, scope: "origin" }}
    />
  ),
};

export const ToolbarNarrow320ManagedProfileReset: Story = {
  args: { scenario: "profile-reset-confirmation" },
  render: () => (
    <NarrowToolbarStory width={320} access={unsharedAccess} resetProfile />
  ),
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const body = within(canvasElement.ownerDocument.body);
    await userEvent.click(
      canvas.getByRole("button", { name: "Browser options" }),
    );
    await userEvent.click(
      await body.findByRole("menuitem", {
        name: "Reset development profile",
      }),
    );
    const dialog = await body.findByRole("alertdialog");
    await waitFor(() => expect(dialog).toBeVisible());
  },
};

export const InspectOff: Story = {
  args: { scenario: "inspect-off" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      canvas.getByRole("button", { name: "Inspect page elements" }),
    ).toBeVisible();
    const btn = canvas.getByRole("button", { name: "Inspect page elements" });
    await expect(btn).toHaveAttribute("aria-pressed", "false");
  },
};

export const InspectOn: Story = {
  args: { scenario: "inspect-on" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      canvas.getByRole("button", { name: "Hide inspect highlights" }),
    ).toBeVisible();
    const btn = canvas.getByRole("button", { name: "Hide inspect highlights" });
    await expect(btn).toHaveAttribute("aria-pressed", "true");
  },
};

export const UnsupportedFrame: Story = {
  args: { scenario: "unsupported-frame" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      canvas.getByRole("button", { name: "Hide inspect highlights" }),
    ).toBeVisible();
    await expect(
      canvas.getByText("Uninspectable frame · human takeover"),
    ).toBeVisible();
  },
};

export const SplitPane: StoryObj<typeof SplitPaneBrowserStory> = {
  render: () => <SplitPaneBrowserStory />,
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByLabelText("Source editor")).toBeVisible();
    await expect(canvas.getByLabelText("Browser split pane")).toBeVisible();
    await expect(
      canvas.getByText("Code agent is using this tab"),
    ).toBeVisible();
  },
};

export const SameDocumentNavigation: Story = {
  args: { scenario: "same-document" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      canvas.getByRole("textbox", { name: "Address or search" }),
    ).toHaveValue("localhost:4173/review/browser?view=replaced#summary");
  },
};

export const InspectEnableFailure: Story = {
  args: { scenario: "inspect-enable-failure" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      canvas.getByText("Could not show inspect highlights"),
    ).toBeVisible();
    await expect(
      canvas.getByRole("button", { name: "Inspect page elements" }),
    ).toHaveAttribute("aria-pressed", "false");
  },
};

export const InspectRemovalFailure: Story = {
  args: { scenario: "inspect-remove-failure" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      canvas.getByText("Could not hide inspect highlights"),
    ).toBeVisible();
    await expect(
      canvas.getByRole("button", { name: "Hide inspect highlights" }),
    ).toHaveAttribute("aria-pressed", "true");
  },
};

export const ViewportAgentControlled: Story = {
  args: {
    scenario: "viewport-desktop",
    viewport: { preset: "desktop", customWidth: 1024 },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      canvas.getByRole("button", { name: /Viewport: Desktop 1440/i }),
    ).toBeVisible();
  },
};

export const ControllerStates: StoryObj<typeof ControlRows> = {
  render: () => <ControlRows />,
};
