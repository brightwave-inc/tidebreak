import type { Meta, StoryObj } from "@storybook/react-vite";

import type {
  CodeDeliveryPullRequestSummary,
  CodeDeliveryStackMember,
  PullRequestDigest,
} from "@/api";
import { Badge } from "@/components/ui/badge";
import {
  PULL_REQUEST_LIFECYCLE_LABEL,
  PULL_REQUEST_LIFECYCLE_TONE,
  PR_GATE_GROUP,
  STATUS_TONE_BADGE_VARIANT,
  checkCounts,
  mergeBlockedReasons,
  prStateChips,
  prStatus,
  pullRequestLifecycle,
  type PrStateInput,
} from "@/code/prState";
import { PrLifecycleIcon, StackMap } from "@/code/PullRequestDetail";
import { STATUS_MARK, STATUS_TEXT, type StatusTone } from "@/code/statusTone";

import { deliveryPullRequests } from "./fixtures";

/**
 * The pull-request state system, in one place: lifecycle colors in GitHub's
 * vocabulary, the gate ladder's headlines, the chips a surface composes from
 * them, the merge-box blocker list, and the stack map. A change to any of
 * those tables is a change the next builder should be able to see here
 * before it ships.
 */

const meta: Meta = {
  title: "Code/PR state",
  parameters: { layout: "padded" },
};

export default meta;

function Section({
  title,
  description,
  children,
}: {
  title: string;
  description: string;
  children: React.ReactNode;
}) {
  return (
    <section className="flex flex-col gap-3">
      <div>
        <h2 className="text-base font-semibold">{title}</h2>
        <p className="text-muted-foreground mt-0.5 text-xs leading-relaxed">
          {description}
        </p>
      </div>
      {children}
    </section>
  );
}

const openDigest: PullRequestDigest = {
  number: 2251,
  state: "open",
  title: "Add the delivery monitor",
};

function digestWith(overrides: Partial<PullRequestDigest>): PullRequestDigest {
  return { ...openDigest, ...overrides };
}

const lifecycleStates: Array<{
  input: PrStateInput;
  note: string;
}> = [
  { input: digestWith({}), note: "open on the host, not a draft" },
  { input: digestWith({ draft: true }), note: "open and marked draft" },
  {
    input: digestWith({ state: "merged", merged: true }),
    note: "state token, or merge evidence",
  },
  {
    input: digestWith({ state: "closed", merged: false }),
    note: "closed without merging",
  },
];

export const Lifecycle: StoryObj = {
  name: "Lifecycle",
  render: () => (
    <Section
      title="Lifecycle"
      description="GitHub's own colors, on every surface: green open, gray draft, purple merged, red closed. A settled pull request is a settled color; nothing downstream may recolor it."
    >
      <div className="flex flex-wrap gap-6">
        {lifecycleStates.map(({ input, note }) => {
          const lifecycle = pullRequestLifecycle(input);
          return (
            <div key={lifecycle} className="flex flex-col items-start gap-2">
              <div className="flex items-center gap-2">
                <PrLifecycleIcon
                  lifecycle={lifecycle}
                  className={cnSize4(
                    STATUS_MARK[PULL_REQUEST_LIFECYCLE_TONE[lifecycle]],
                  )}
                />
                <Badge
                  variant={
                    STATUS_TONE_BADGE_VARIANT[
                      PULL_REQUEST_LIFECYCLE_TONE[lifecycle]
                    ]
                  }
                  size="sm"
                >
                  {PULL_REQUEST_LIFECYCLE_LABEL[lifecycle]}
                </Badge>
              </div>
              <p className="text-muted-foreground max-w-40 text-xs">{note}</p>
            </div>
          );
        })}
      </div>
    </Section>
  ),
};

function cnSize4(toneClass: string): string {
  return `size-4 shrink-0 ${toneClass}`;
}

const chipStates: Array<{ label: string; input: PrStateInput }> = [
  { label: "Open, no review yet", input: digestWith({}) },
  {
    label: "Open, approved",
    input: digestWith({ review_decision: "approved" }),
  },
  {
    label: "Open, changes requested",
    input: digestWith({ review_decision: "changes_requested" }),
  },
  {
    label: "Open, review required",
    input: digestWith({
      review_decision: "review_required",
      merge_state_status: "blocked",
    }),
  },
  {
    label: "Open, in the merge queue",
    input: digestWith({ in_merge_queue: true }),
  },
  {
    label: "Open, auto-merge armed",
    input: digestWith({ auto_merge_enabled: true }),
  },
  {
    label: "Draft",
    input: digestWith({ draft: true }),
  },
  {
    label: "Merged",
    input: digestWith({ state: "merged", merged: true }),
  },
];

export const Chips: StoryObj = {
  name: "Chips",
  render: () => (
    <Section
      title="State chips"
      description="Lifecycle first, then the review verdict, then queue or auto-merge. Queue membership is its own chip in info blue — a queued pull request is still open, and a single chip that says only “Queued” erases that."
    >
      <div className="flex flex-col gap-2.5">
        {chipStates.map(({ label, input }) => (
          <div key={label} className="flex items-center gap-3">
            <span className="w-56 shrink-0 text-xs text-muted-foreground">
              {label}
            </span>
            <div className="flex flex-wrap items-center gap-1">
              {prStateChips(input).map((chip) => (
                <Badge
                  key={chip.key}
                  variant={STATUS_TONE_BADGE_VARIANT[chip.tone]}
                  size="sm"
                >
                  {chip.label}
                </Badge>
              ))}
            </div>
          </div>
        ))}
      </div>
    </Section>
  ),
};

const gateCases: Array<{ input: PrStateInput }> = [
  {
    input: digestWith({
      mergeable: "conflicting",
      merge_state_status: "dirty",
    }),
  },
  { input: digestWith({ review_decision: "changes_requested" }) },
  {
    input: digestWith({ checks: [{ name: "ci", bucket: "fail" }] }),
  },
  { input: digestWith({ in_merge_queue: true }) },
  { input: digestWith({ merge_state_status: "behind" }) },
  {
    input: digestWith({ checks: [{ name: "ci", bucket: "pending" }] }),
  },
  {
    input: digestWith({
      review_decision: "review_required",
      merge_state_status: "blocked",
    }),
  },
  { input: digestWith({ merge_state_status: "blocked" }) },
  { input: digestWith({ auto_merge_enabled: true }) },
  {
    input: digestWith({ mergeable: "mergeable", merge_state_status: "clean" }),
  },
  { input: digestWith({}) },
  { input: digestWith({ draft: true }) },
  { input: digestWith({ state: "merged", merged: true }) },
  { input: digestWith({ state: "closed" }) },
];

export const Gate: StoryObj = {
  name: "Gate ladder",
  render: () => (
    <Section
      title="Gate ladder"
      description="The one answer to “what stands between this pull request and its base branch?”, in the order a reader resolves them. Every surface — workspace header, review panel, delivery row — reads the same headline from the same table."
    >
      <table className="w-full max-w-3xl text-left text-xs">
        <thead className="text-muted-foreground border-border-subtle border-b">
          <tr>
            <th className="py-1.5 pr-4 font-medium">State</th>
            <th className="py-1.5 pr-4 font-medium">Headline</th>
            <th className="py-1.5 pr-4 font-medium">Group</th>
            <th className="py-1.5 font-medium">Example input</th>
          </tr>
        </thead>
        <tbody>
          {gateCases.map(({ input }, index) => {
            const status = prStatus(input);
            return (
              <tr
                key={index}
                className="border-border-subtle border-b last:border-0"
              >
                <td className="py-1.5 pr-4 font-mono">{status.gate}</td>
                <td
                  className={`py-1.5 pr-4 font-medium ${STATUS_TEXT[status.headline.tone]}`}
                >
                  {status.headline.label}
                </td>
                <td className="text-muted-foreground py-1.5 pr-4">
                  {status.group}
                </td>
                <td className="text-muted-foreground py-1.5 font-mono">
                  {gateInputSummary(input)}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </Section>
  ),
};

function gateInputSummary(input: PrStateInput): string {
  const parts: string[] = [];
  if (input.draft) parts.push("draft");
  if (input.in_merge_queue) parts.push("queued");
  if (input.mergeable) parts.push(input.mergeable);
  if (input.merge_state_status) parts.push(input.merge_state_status);
  if (input.review_decision) parts.push(input.review_decision);
  if (input.auto_merge_enabled) parts.push("auto-merge");
  const counts = checkCounts(input);
  if (counts.failing > 0) parts.push(`${counts.failing} failing`);
  if (counts.pending > 0) parts.push(`${counts.pending} pending`);
  return parts.join(", ") || "bare open pull request";
}

const blockersInput: PrStateInput = digestWith({
  mergeable: "conflicting",
  review_decision: "changes_requested",
  checks: [
    { name: "ci / rust", bucket: "fail" },
    { name: "preview", bucket: "pending" },
  ],
});

export const Blockers: StoryObj = {
  name: "Merge blockers",
  render: () => (
    <Section
      title="Merge blockers"
      description="The merge box lists every blocker in the words GitHub uses, not just the first. The headline picks one; the box keeps the whole story."
    >
      <div className="border-border-subtle bg-muted/20 w-full max-w-xl rounded-lg border p-3">
        <p className="text-xs font-medium">Merge pull request</p>
        <ul className="mt-1.5 flex flex-col gap-1">
          {mergeBlockedReasons(blockersInput).map((reason) => (
            <li
              key={reason}
              className="text-muted-foreground text-xs leading-4"
            >
              {reason}
            </li>
          ))}
        </ul>
      </div>
    </Section>
  ),
};

const stackMembers: CodeDeliveryStackMember[] = [
  {
    number: 2267,
    state: "closed",
    draft: false,
    merged_at: "2026-08-19T20:06:39Z",
    head_branch: "thet/desktop-remote-mode-authority",
    head_sha: "925a8e5e",
  },
  {
    number: 2269,
    state: "open",
    draft: false,
    head_branch: "thet/desktop-remote-mode-ui",
    head_sha: "ea2a9be5",
  },
  {
    number: 2271,
    state: "open",
    draft: true,
    head_branch: "thet/desktop-remote-mode-polish",
    head_sha: "3f0c1b2a",
  },
];

export const Stack: StoryObj = {
  name: "Stack map",
  render: () => (
    <Section
      title="Stack map"
      description="The host-reported stack chain, bottom to top, the way GitHub pins it to a stacked pull request. The layer behind the sheet carries its own ring; merged layers keep their settled color."
    >
      <div className="flex flex-col gap-4">
        <div>
          <p className="text-muted-foreground mb-1.5 text-xs">
            Standing on the middle layer (#2269)
          </p>
          <StackMap
            stack={stackMembers}
            currentNumber={2269}
            url="https://github.com/brightwave-inc/tidebreak/pull/2269"
          />
        </div>
        <div>
          <p className="text-muted-foreground mb-1.5 text-xs">
            Standing on the top layer (#2271)
          </p>
          <StackMap
            stack={stackMembers}
            currentNumber={2271}
            url="https://github.com/brightwave-inc/tidebreak/pull/2271"
          />
        </div>
      </div>
    </Section>
  ),
};

export const DeliveryRows: StoryObj = {
  name: "Delivery rows",
  render: () => {
    const grouped = new Map<string, CodeDeliveryPullRequestSummary[]>();
    for (const item of deliveryPullRequests) {
      const status = prStatus(item);
      const list = grouped.get(status.group) ?? [];
      list.push(item);
      grouped.set(status.group, list);
    }
    const groupOrder = [
      "attention",
      "ready",
      "waiting",
      "handed_off",
      "draft",
      "done",
    ] as const;
    return (
      <Section
        title="Delivery rows, by group"
        description="The typed delivery fixtures run through the same classifier the page uses. One row per fixture, grouped by who owns the next move — the same table the workspace header and the review panel read."
      >
        <div className="flex flex-col gap-5">
          {groupOrder
            .filter((group) => grouped.has(group))
            .map((group) => (
              <div key={group} className="flex flex-col gap-1.5">
                <p className="text-muted-foreground text-2xs font-medium uppercase tracking-wide">
                  {group} ({grouped.get(group)!.length})
                </p>
                {grouped.get(group)!.map((item) => {
                  const status = prStatus(item);
                  return (
                    <div
                      key={item.id}
                      className="border-border-subtle flex items-center gap-2.5 rounded-md border px-3 py-2"
                    >
                      <PrLifecycleIcon
                        lifecycle={status.lifecycle}
                        className={cnSize4(
                          STATUS_MARK[status.headline.tone as StatusTone],
                        )}
                      />
                      <span className="min-w-0 flex-1 truncate text-sm">
                        #{item.number} {item.title}
                      </span>
                      <span
                        className={`text-xs font-medium ${STATUS_TEXT[status.headline.tone]}`}
                      >
                        {status.headline.label}
                      </span>
                      <span className="text-muted-foreground w-20 text-right text-2xs">
                        {PR_GATE_GROUP[status.gate]}
                      </span>
                    </div>
                  );
                })}
              </div>
            ))}
        </div>
      </Section>
    );
  },
};
