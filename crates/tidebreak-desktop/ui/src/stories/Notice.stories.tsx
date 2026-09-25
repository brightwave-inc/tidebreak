import type { Meta, StoryObj } from "@storybook/react-vite";
import { X } from "lucide-react";
import { expect, fn, userEvent, within } from "storybook/test";

import { Button } from "@/components/ui/button";
import {
  Notice,
  NoticeDetail,
  NoticeRetryButton,
  type NoticeTone,
} from "@/components/ui/notice";
import { friendlyErrorMessage } from "@/lib/utils";
import { failureFixtures } from "./fixtures";

const meta = {
  title: "Components/Notice",
  component: Notice,
  parameters: { layout: "padded" },
} satisfies Meta<typeof Notice>;

export default meta;
type Story = StoryObj<typeof meta>;

/** One sentence for each tone, the way the product words it. */
const TONE_COPY: Record<NoticeTone, { title: string; body: string }> = {
  critical: {
    title: "Could not load your apps",
    body: friendlyErrorMessage(failureFixtures.unreachable, "Try again."),
  },
  warning: {
    title: "The coding engine check did not answer",
    body: "Add a repository now, and check the engines again when they respond.",
  },
  info: {
    title: "This work is archived",
    body: "It stays out of your list until you unarchive it or send a message.",
  },
  success: {
    title: "Voice input is ready",
    body: "The selected model transcribes recordings on this computer.",
  },
  neutral: {
    title: "Not signed in",
    body: "Sign in to a model gateway to use the models your team shares.",
  },
};

const TONES: NoticeTone[] = [
  "critical",
  "warning",
  "info",
  "success",
  "neutral",
];

function Column({ children }: { children: React.ReactNode }) {
  return (
    <div className="mx-auto flex w-full max-w-3xl flex-col gap-3">
      {children}
    </div>
  );
}

/** Every tone, with a verdict and the sentence that says what to do next. */
export const Tones: Story = {
  render: () => (
    <Column>
      {TONES.map((tone) => (
        <Notice key={tone} tone={tone} title={TONE_COPY[tone].title}>
          {TONE_COPY[tone].body}
        </Notice>
      ))}
    </Column>
  ),
};

/** The action slot: a panel failure's Try again, and the other recoveries. */
export const WithAction: Story = {
  render: () => (
    <Column>
      <Notice
        tone="critical"
        title={TONE_COPY.critical.title}
        action={<NoticeRetryButton onClick={fn()} />}
      >
        {TONE_COPY.critical.body}
      </Notice>
      <Notice
        tone="warning"
        title={TONE_COPY.warning.title}
        action={
          // A retry that is running waits for its answer.
          <NoticeRetryButton pending onClick={fn()}>
            Re-checking…
          </NoticeRetryButton>
        }
      >
        {TONE_COPY.warning.body}
      </Notice>
      <Notice
        tone="info"
        action={
          <Button type="button" size="sm" variant="outline" onClick={fn()}>
            Unarchive
          </Button>
        }
      >
        {TONE_COPY.info.body}
      </Notice>
      <Notice
        tone="neutral"
        action={
          <Button
            type="button"
            size="icon-sm"
            variant="ghost"
            aria-label="Dismiss"
            onClick={fn()}
          >
            <X aria-hidden="true" />
          </Button>
        }
      >
        The rest of this conversation uses the gateway's default model.
      </Notice>
    </Column>
  ),
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const retry = canvas.getAllByRole("button", { name: "Try again" })[0];
    await userEvent.tab();
    await expect(retry).toHaveFocus();
  },
};

/** A message and its body alone, when there is nothing to do but read. */
export const WithoutAction: Story = {
  render: () => (
    <Column>
      <Notice tone="critical">
        {friendlyErrorMessage(failureFixtures.notFound, "Try again.", {
          not_found: "This app is no longer in your library.",
        })}
      </Notice>
      <Notice tone="warning">
        This machine has no clone destination configured. An administrator sets
        one on the machine.
      </Notice>
    </Column>
  ),
};

/** Long copy and machine detail wrap inside the notice; nothing overflows. */
export const LongText: Story = {
  render: () => (
    <Column>
      <Notice
        tone="critical"
        title="Could not push feature/notices to origin because the remote has commits this workspace does not"
        action={<NoticeRetryButton onClick={fn()} />}
      >
        <p>
          Pull the remote changes into this workspace, resolve any conflicts,
          then push again. Nothing on the remote changed.
        </p>
        <NoticeDetail>
          {friendlyErrorMessage(failureFixtures.longDetail, "Try again.")}
        </NoticeDetail>
      </Notice>
      <Notice tone="info">
        {"Unable to read /workspace/" +
          "long-directory-name/".repeat(8) +
          "output.log because the path is outside the workspace root."}
      </Notice>
    </Column>
  ),
  play: async ({ canvasElement }) => {
    for (const notice of canvasElement.querySelectorAll<HTMLElement>(
      '[data-slot="notice"]',
    )) {
      await expect(notice.scrollWidth).toBeLessThanOrEqual(notice.clientWidth);
    }
  },
};

/** In a narrow panel the action wraps under the message instead of squeezing it. */
export const NarrowPanel: Story = {
  render: () => (
    <div className="flex w-80 max-w-full flex-col gap-3 rounded-xl border border-border bg-background p-3">
      <p className="text-sm font-medium">Outputs</p>
      <Notice
        tone="critical"
        title="Could not load this conversation's outputs"
        action={<NoticeRetryButton onClick={fn()} />}
      >
        {friendlyErrorMessage(failureFixtures.unavailable, "Try again.")}
      </Notice>
      <Notice
        tone="warning"
        density="compact"
        action={<NoticeRetryButton size="xs" onClick={fn()} />}
      >
        GitHub repository discovery is stale.
      </Notice>
    </div>
  ),
  play: async ({ canvasElement }) => {
    const notice = canvasElement.querySelector<HTMLElement>(
      '[data-slot="notice"]',
    );
    const action = notice?.querySelector<HTMLElement>(
      '[data-slot="notice-action"]',
    );
    const message = action?.previousElementSibling as HTMLElement | null;
    // Wrapped: the action starts on its own line, under the message.
    await expect(action?.getBoundingClientRect().top).toBeGreaterThanOrEqual(
      message?.getBoundingClientRect().bottom ?? 0,
    );
  },
};

/**
 * Docked to a bar inside a pane, as a strip across it, where the app docks
 * them: under a file's header, and under the browser's address bar.
 */
export const Docked: Story = {
  render: () => (
    <div className="mx-auto flex w-full max-w-3xl flex-col gap-4">
      <div className="flex h-60 flex-col overflow-hidden rounded-xl border border-border bg-background">
        <div className="flex h-9 shrink-0 items-center border-b border-border px-3 text-sm font-medium">
          src/main.rs
        </div>
        <Notice
          tone="warning"
          docked="top"
          role="alert"
          action={
            <>
              <Button type="button" size="sm" variant="outline" onClick={fn()}>
                Reload
              </Button>
              <Button type="button" size="sm" variant="outline" onClick={fn()}>
                Keep my changes
              </Button>
            </>
          }
        >
          This file changed on disk.
        </Notice>
        <pre className="min-h-0 flex-1 overflow-auto p-3 font-mono text-xs text-muted-foreground">
          {'fn main() {\n    println!("hello");\n}'}
        </pre>
      </div>
      <div className="flex h-44 flex-col overflow-hidden rounded-xl border border-border bg-background">
        <div className="flex h-10 shrink-0 items-center px-2">
          <div className="flex h-7 min-w-0 flex-1 items-center rounded-md border border-critical-border bg-background px-2 font-mono text-xs">
            localhost:30o0
          </div>
        </div>
        <Notice tone="critical" docked="bottom" density="compact">
          That address is not a valid URL.
        </Notice>
        <div className="min-h-0 flex-1 bg-page-background" />
      </div>
    </div>
  ),
};

/** The dense size, for a transcript note or a composer's failure line. */
export const Compact: Story = {
  render: () => (
    <Column>
      <Notice
        tone="neutral"
        density="compact"
        action={<NoticeRetryButton size="xs" onClick={fn()} />}
      >
        You stopped this response.
      </Notice>
      <Notice tone="critical" density="compact">
        {`Couldn’t attach image: ${friendlyErrorMessage(failureFixtures.conflict, "Try again.")}`}
      </Notice>
      <Notice tone="warning" density="compact">
        The model declined to continue. Rephrase the request, or try another
        model.
      </Notice>
    </Column>
  ),
};

/**
 * What each kind of failure reads as once `friendlyErrorMessage` has worded
 * it: no class names, no status codes, and renderer copy where the server's
 * text was written for a log.
 */
export const FailureCopy: Story = {
  render: () => (
    <Column>
      {(
        [
          ["Network", failureFixtures.unreachable, undefined],
          ["Server restarting", failureFixtures.unavailable, undefined],
          // The caller knows what "not found" means here, so it says so.
          [
            "Not found, worded by the caller",
            failureFixtures.notFound,
            { not_found: "This file is no longer in the project." },
          ],
          ["Server refusal", failureFixtures.conflict, undefined],
        ] as const
      ).map(([label, failure, kindCopy]) => (
        <section
          key={label}
          aria-label={label}
          className="flex flex-col gap-1.5"
        >
          <p className="text-xs font-medium text-muted-foreground">{label}</p>
          <Notice
            tone="critical"
            title="Could not load the project's files"
            action={<NoticeRetryButton onClick={fn()} />}
          >
            {friendlyErrorMessage(failure, "Try again in a moment.", kindCopy)}
          </Notice>
        </section>
      ))}
    </Column>
  ),
  play: async ({ canvasElement }) => {
    await expect(canvasElement.textContent).not.toMatch(
      /HttpError|TypeError|Load failed|\b50[0-9]\b|\b404\b/,
    );
  },
};
