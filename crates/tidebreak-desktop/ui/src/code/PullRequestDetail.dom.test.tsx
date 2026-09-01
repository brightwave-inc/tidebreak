// @vitest-environment jsdom
import { StrictMode } from "react";
import {
  cleanup,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { toast } from "sonner";
import { afterEach, describe, expect, it, vi } from "vitest";

import type {
  CodeDeliveryPullRequestActionBody,
  CodeDeliveryPullRequestTarget,
} from "../api/types";
import {
  deliveryPullRequestDetails,
  deliveryPullRequests,
  stackedDeliveryPullRequests,
  unregisteredDeliveryPullRequests,
} from "../stories/fixtures";
import { PullRequestDetailSheet } from "./PullRequestDetail";

afterEach(cleanup);

vi.mock("@/openInBrowser", () => ({ openInBrowser: vi.fn() }));
vi.mock("sonner", () => ({
  toast: { success: vi.fn(), warning: vi.fn(), error: vi.fn() },
}));

function client(overrides: Partial<Record<string, unknown>> = {}) {
  return {
    getCodeDeliveryPullRequestDetail: async ({
      number,
    }: CodeDeliveryPullRequestTarget) => {
      const detail = deliveryPullRequestDetails[number];
      if (!detail) throw new Error(`no fixture for #${number}`);
      return detail;
    },
    runCodeDeliveryPullRequestAction: async (
      _body: CodeDeliveryPullRequestActionBody,
    ) => ({ success: true, message: "Done." }),
    ...overrides,
  } as never;
}

function summaryFor(number: number) {
  const summary = deliveryPullRequests.find((item) => item.number === number);
  if (!summary) throw new Error(`no fixture for #${number}`);
  return summary;
}

async function renderPanel(number: number, api = client()) {
  render(
    <PullRequestDetailSheet
      client={api}
      summary={summaryFor(number)}
      onClose={vi.fn()}
      onChanged={vi.fn()}
      onOpenWorkspace={vi.fn()}
    />,
  );
  await screen.findByRole("tab", { name: /Conversation/ });
}

describe("PullRequestDetailSheet", () => {
  it("accepts load, refresh, and mutation completions in Strict Mode", async () => {
    vi.mocked(toast.success).mockClear();
    const baseDetail = deliveryPullRequestDetails[2251]!;
    const refreshedDetail = {
      ...baseDetail,
      errors: [
        {
          kind: "detail",
          message: "Strict Mode refresh completed.",
        },
      ],
    };
    const mutatedDetail = {
      ...baseDetail,
      errors: [
        {
          kind: "detail",
          message: "Strict Mode mutation refresh completed.",
        },
      ],
    };
    let phase: "load" | "refresh" | "mutation" = "load";
    const getDetail = vi.fn(async () => {
      if (phase === "refresh") return refreshedDetail;
      if (phase === "mutation") return mutatedDetail;
      return baseDetail;
    });
    const runAction = vi.fn(async () => ({
      success: true,
      message: "Posted.",
    }));
    const onChanged = vi.fn();

    render(
      <StrictMode>
        <PullRequestDetailSheet
          client={client({
            getCodeDeliveryPullRequestDetail: getDetail,
            runCodeDeliveryPullRequestAction: runAction,
          })}
          summary={summaryFor(2251)}
          onClose={vi.fn()}
          onChanged={onChanged}
          onOpenWorkspace={vi.fn()}
        />
      </StrictMode>,
    );

    expect(
      await screen.findByRole("tab", { name: /Conversation/ }),
    ).toBeInTheDocument();
    phase = "refresh";
    await userEvent.click(screen.getByRole("button", { name: "Refresh" }));
    expect(
      await screen.findByText("Strict Mode refresh completed."),
    ).toBeInTheDocument();

    phase = "mutation";
    await userEvent.type(
      screen.getByLabelText("Comment on this pull request"),
      "Strict Mode comment",
    );
    await userEvent.click(screen.getByRole("button", { name: "Comment" }));

    expect(
      await screen.findByText("Strict Mode mutation refresh completed."),
    ).toBeInTheDocument();
    expect(onChanged).toHaveBeenCalledTimes(1);
    expect(toast.success).toHaveBeenCalledWith("Posted.");
  });

  it("names the lifecycle instead of leaving a settled pull request unlabeled", async () => {
    await renderPanel(2240);
    expect(await screen.findByText("Merged")).toBeInTheDocument();
    expect(screen.getByText(/merged .* by devon/)).toBeInTheDocument();
    // Nothing left to merge, and a merged pull request cannot be reopened.
    expect(screen.queryByRole("button", { name: "Merge" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Reopen" })).toBeNull();
  });

  it("offers only reopen on a pull request closed without merging", async () => {
    await renderPanel(309);
    expect(await screen.findByText("Closed")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Reopen" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Merge" })).toBeNull();
  });

  it("renders the description as Markdown rather than raw source", async () => {
    await renderPanel(2251);
    // The fixture body opens with "## Summary". Rendering it as text was the
    // bug: the panel showed the hashes.
    expect(
      await screen.findByRole("heading", { name: "Summary" }),
    ).toBeInTheDocument();
    expect(screen.queryByText(/^## Summary/)).toBeNull();
  });

  it("explains a blocked merge instead of letting the API refuse it", async () => {
    await renderPanel(2229);
    expect(
      await screen.findByText(
        "Resolve the conflicts with the base branch first.",
      ),
    ).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Merge" })).toBeNull();
    expect(
      screen.queryByRole("button", { name: "Enable auto-merge" }),
    ).toBeNull();
  });

  it("shows the changed files with their diffs", async () => {
    await renderPanel(2251);
    await userEvent.click(screen.getByRole("tab", { name: /Files/ }));
    expect(await screen.findByText(/19 files changed/)).toBeInTheDocument();

    // MiddleTruncate splits a long path across two spans and carries the whole
    // path as the title, so match on that rather than on a text node.
    await userEvent.click(
      screen.getByTitle(
        "crates/tidebreak-desktop/ui/src/code/pullRequestPresentation.ts",
      ),
    );
    expect(await screen.findByText(/@@ -0,0 \+1,8 @@/)).toBeInTheDocument();

    // A binary file says so rather than showing an empty diff.
    await userEvent.click(screen.getByTitle("docs/assets/delivery-center.png"));
    expect(await screen.findByText(/No text diff/)).toBeInTheDocument();
  });

  it("lists every check, failures first", async () => {
    await renderPanel(2251);
    const checksTab = screen.getByRole("tab", { name: /Checks/ });
    expect(checksTab).toHaveTextContent("2/2");
    await userEvent.click(checksTab);
    // The same words the delivery row uses for these counts.
    const panel = await screen.findByRole("tabpanel");
    expect(within(panel).getByText("1 failed")).toBeInTheDocument();
    const rows = screen.getAllByRole("button", { name: /desktop \// });
    expect(rows[0]).toHaveTextContent("desktop / storybook");
  });

  it("counts skipped checks as terminal tab progress", async () => {
    await renderPanel(2240);
    const checksTab = await screen.findByRole("tab", { name: /Checks/ });
    expect(checksTab).toHaveTextContent("3/3");
    await userEvent.click(checksTab);
    const panel = await screen.findByRole("tabpanel");
    expect(within(panel).getByText("2 passed")).toBeInTheDocument();
  });

  it("keeps pending checks out of progress and uses the running mark", async () => {
    await renderPanel(2229);
    const checksTab = await screen.findByRole("tab", { name: /Checks/ });
    expect(checksTab).toHaveTextContent("1/2");
    await userEvent.click(checksTab);
    const pending = screen.getByRole("button", { name: /desktop \/ lint/ });
    expect(pending.querySelector(".lucide-circle-dashed")).not.toBeNull();
    expect(pending.querySelector(".lucide-play")).toBeNull();
  });

  it("posts a comment and clears the box", async () => {
    const runAction = vi.fn(
      async (_body: CodeDeliveryPullRequestActionBody) => ({
        success: true,
        message: "Posted.",
      }),
    );
    await renderPanel(
      2251,
      client({ runCodeDeliveryPullRequestAction: runAction }),
    );

    const box = screen.getByLabelText("Comment on this pull request");
    await userEvent.type(box, "Looks good.");
    await userEvent.click(screen.getByRole("button", { name: "Comment" }));

    await waitFor(() => expect(runAction).toHaveBeenCalledTimes(1));
    expect(runAction.mock.calls[0]![0]).toMatchObject({
      action: { type: "comment", body: "Looks good." },
    });
    await waitFor(() => expect(box).toHaveValue(""));
  });

  it("warns when only some workflow reruns start", async () => {
    vi.mocked(toast.success).mockClear();
    vi.mocked(toast.warning).mockClear();
    const runAction = vi.fn(
      async (_body: CodeDeliveryPullRequestActionBody) => ({
        success: false,
        message:
          "Failed jobs queued for one workflow run; one workflow run failed",
        rerun_outcomes: [
          { workflow_run_id: 4401, success: true },
          { workflow_run_id: 4402, success: false, error: "HTTP 503" },
        ],
      }),
    );
    await renderPanel(
      2251,
      client({ runCodeDeliveryPullRequestAction: runAction }),
    );

    await userEvent.click(screen.getByRole("button", { name: "Rerun failed" }));

    await waitFor(() =>
      expect(toast.warning).toHaveBeenCalledWith(
        "Failed jobs queued for one workflow run; one workflow run failed",
        {
          description: expect.stringContaining(
            "Queued: desktop / storybook (run 4401).",
          ),
        },
      ),
    );
    expect(vi.mocked(toast.warning).mock.calls[0]?.[1]).toEqual({
      description: expect.stringContaining("Failed: Run 4402: HTTP 503."),
    });
    expect(toast.success).not.toHaveBeenCalled();
  });

  it("ignores a mutation completion after the selected pull request changes", async () => {
    vi.mocked(toast.success).mockClear();
    let resolveAction!: (value: { success: boolean; message: string }) => void;
    const action = new Promise<{ success: boolean; message: string }>(
      (resolve) => {
        resolveAction = resolve;
      },
    );
    const runAction = vi.fn(() => action);
    const onChanged = vi.fn();
    const api = client({ runCodeDeliveryPullRequestAction: runAction });
    const props = {
      client: api,
      onClose: vi.fn(),
      onChanged,
      onOpenWorkspace: vi.fn(),
    };
    const view = render(
      <PullRequestDetailSheet
        {...props}
        summary={summaryFor(2251)}
        initialDetail={deliveryPullRequestDetails[2251]}
      />,
    );

    const box = await screen.findByLabelText("Comment on this pull request");
    await userEvent.type(box, "Slow comment");
    await userEvent.click(screen.getByRole("button", { name: "Comment" }));
    await waitFor(() => expect(runAction).toHaveBeenCalledTimes(1));

    view.rerender(
      <PullRequestDetailSheet
        {...props}
        summary={summaryFor(2247)}
        initialDetail={deliveryPullRequestDetails[2247]}
      />,
    );
    resolveAction({ success: true, message: "Posted." });

    expect(
      await screen.findByRole("heading", {
        name: "Make workspace deep links durable",
      }),
    ).toBeInTheDocument();
    await waitFor(() => expect(onChanged).not.toHaveBeenCalled());
    expect(toast.success).not.toHaveBeenCalled();
  });

  it("shows partial detail errors instead of presenting an empty read as complete", async () => {
    const detail = {
      ...deliveryPullRequestDetails[2251]!,
      comments: [],
      files: [],
      errors: [
        {
          kind: "detail",
          message: "Could not load reviews: HTTP 503",
        },
      ],
    };
    await renderPanel(
      2251,
      client({ getCodeDeliveryPullRequestDetail: async () => detail }),
    );

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Could not load reviews: HTTP 503",
    );
  });

  it("keeps the comment button inert until there is something to say", async () => {
    await renderPanel(2251);
    expect(screen.getByRole("button", { name: "Comment" })).toBeDisabled();
  });

  it("carries labels, reviewers, and the diffstat in the header", async () => {
    await renderPanel(2251);
    const header = await screen.findByLabelText("Pull request summary");
    expect(within(header).getByText("desktop")).toBeInTheDocument();
    expect(within(header).getByText("+2140")).toBeInTheDocument();
    expect(within(header).getByText("−83")).toBeInTheDocument();
    expect(within(header).getByText("devon")).toBeInTheDocument();
  });

  /**
   * The confirmation is inline in the actions card, never a second Radix
   * modal: the sheet is already one, and stacked modals share a dismiss
   * layer and the body pointer-events lock.
   */
  it("shows only the host merge action that is available", async () => {
    await renderPanel(2247);
    expect(screen.getByRole("button", { name: "Merge" })).toBeEnabled();
    expect(
      screen.queryByRole("button", { name: "Enable auto-merge" }),
    ).toBeNull();
    expect(
      screen.queryByRole("button", { name: "Merge when ready" }),
    ).toBeNull();
  });

  it("admin-merges only through the inline confirmation, with the bypass flag", async () => {
    const runAction = vi.fn(
      async (_body: CodeDeliveryPullRequestActionBody) => ({
        success: true,
        message: "Merged.",
      }),
    );
    await renderPanel(
      2251,
      client({ runCodeDeliveryPullRequestAction: runAction }),
    );

    // Direct merge is not available; the host action is auto-merge instead.
    expect(screen.queryByRole("button", { name: "Merge" })).toBeNull();
    expect(
      screen.getByRole("button", { name: "Enable auto-merge" }),
    ).toBeEnabled();
    await userEvent.click(
      screen.getByRole("button", { name: "More pull request actions" }),
    );
    await userEvent.click(
      await screen.findByRole("menuitem", {
        name: /Admin merge \(bypass protections\)/,
      }),
    );
    expect(runAction).not.toHaveBeenCalled();
    expect(screen.queryByRole("alertdialog")).toBeNull();

    await userEvent.click(screen.getByRole("button", { name: "Admin merge" }));
    await waitFor(() => expect(runAction).toHaveBeenCalledTimes(1));
    expect(runAction.mock.calls[0]![0]).toMatchObject({
      action: {
        type: "merge",
        admin: true,
        auto: false,
        expected_head_sha: "82ab990",
      },
    });
  });

  it("abandons the admin merge on cancel", async () => {
    const runAction = vi.fn(
      async (_body: CodeDeliveryPullRequestActionBody) => ({
        success: true,
        message: "Merged.",
      }),
    );
    await renderPanel(
      2251,
      client({ runCodeDeliveryPullRequestAction: runAction }),
    );

    await userEvent.click(
      screen.getByRole("button", { name: "More pull request actions" }),
    );
    await userEvent.click(
      await screen.findByRole("menuitem", {
        name: /Admin merge \(bypass protections\)/,
      }),
    );
    await userEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("button", { name: "Admin merge" })).toBeNull();
    expect(runAction).not.toHaveBeenCalled();
  });
});

describe("merge stack", () => {
  function stackedSummary(number: number) {
    const summary = stackedDeliveryPullRequests.find(
      (item) => item.number === number,
    );
    if (!summary) throw new Error(`no stacked fixture for #${number}`);
    return summary;
  }

  async function renderStackPanel(
    number: number,
    api = client(),
    hasMergeQueue = false,
  ) {
    render(
      <PullRequestDetailSheet
        client={api}
        summary={stackedSummary(number)}
        hasMergeQueue={hasMergeQueue}
        onClose={vi.fn()}
        onChanged={vi.fn()}
        onOpenWorkspace={vi.fn()}
      />,
    );
    await screen.findByRole("tab", { name: /Conversation/ });
  }

  async function confirmStackMerge() {
    await userEvent.click(
      screen.getByRole("button", { name: /Merge stack \(\d layers\)/ }),
    );
    await userEvent.click(screen.getByRole("button", { name: "Merge stack" }));
  }

  it("merges the opened pull request and the layers below it, in order", async () => {
    vi.mocked(toast.success).mockClear();
    const runAction = vi.fn(
      async (_body: CodeDeliveryPullRequestActionBody) => ({
        success: true,
        message: "Merged.",
      }),
    );
    await renderStackPanel(
      2302,
      client({ runCodeDeliveryPullRequestAction: runAction }),
    );

    await confirmStackMerge();

    // Bottom layer first, then the opened layer, each a direct merge against
    // that layer's own head.
    expect(runAction).toHaveBeenCalledTimes(2);
    const calls = runAction.mock.calls.map((call) => call[0]);
    expect(calls.map((call) => call.target.number)).toEqual([2301, 2302]);
    for (const call of calls) {
      expect(call.action).toMatchObject({ type: "merge", auto: false });
    }
    expect(toast.success).toHaveBeenCalledWith("Stack merged.");
  });

  it("replaces the single-layer merge while the stack offer stands", async () => {
    await renderStackPanel(2302);
    expect(
      screen.getByRole("button", { name: /Merge stack \(2 layers\)/ }),
    ).toBeInTheDocument();
    // A single-layer squash merge would land #2302 into #2301's branch — the
    // half-measure the stack exists to avoid.
    expect(
      screen.queryByRole("button", { name: "Squash and merge" }),
    ).toBeNull();
  });

  it("keeps the single-layer merge for a bottom layer with nothing below it", async () => {
    await renderStackPanel(2301);
    expect(screen.queryByRole("button", { name: /Merge stack/ })).toBeNull();
    // The single-layer merge stays: nothing below this layer to land with it.
    expect(screen.getByRole("button", { name: "Merge" })).toBeInTheDocument();
  });

  it("does not offer auto-merge for a stacked pull request", async () => {
    const stackedPending = {
      ...stackedDeliveryPullRequests[1]!,
      checks: [{ name: "ci", bucket: "pending" as const }],
      mergeable: "unknown",
      merge_state_status: "blocked",
    };
    await render(
      <PullRequestDetailSheet
        client={client({
          getCodeDeliveryPullRequestDetail: async () => ({
            ...deliveryPullRequestDetails[2302]!,
            summary: stackedPending,
            stack: undefined,
          }),
        })}
        summary={stackedPending}
        hasMergeQueue={false}
        onClose={vi.fn()}
        onChanged={vi.fn()}
        onOpenWorkspace={vi.fn()}
      />,
    );
    await screen.findByRole("tab", { name: /Conversation/ });
    expect(
      screen.queryByRole("button", { name: "Enable auto-merge" }),
    ).toBeNull();
  });

  it("stops at a layer the merge gate refuses, merging nothing", async () => {
    vi.mocked(toast.warning).mockClear();
    const runAction = vi.fn(
      async (_body: CodeDeliveryPullRequestActionBody) => ({
        success: true,
        message: "Merged.",
      }),
    );
    const blockedDetail = {
      ...deliveryPullRequestDetails[2301]!,
      summary: {
        ...stackedDeliveryPullRequests[0]!,
        mergeable: "conflicting",
        merge_state_status: "dirty",
      },
    };
    const getDetail = vi.fn(
      async ({ number }: CodeDeliveryPullRequestTarget) =>
        number === 2301 ? blockedDetail : deliveryPullRequestDetails[number]!,
    );
    await renderStackPanel(
      2302,
      client({
        getCodeDeliveryPullRequestDetail: getDetail,
        runCodeDeliveryPullRequestAction: runAction,
      }),
    );

    await confirmStackMerge();

    expect(runAction).not.toHaveBeenCalled();
    expect(toast.warning).toHaveBeenCalledWith(
      expect.stringMatching(/stopped at #2301.*conflicts/),
    );
    expect(toast.success).not.toHaveBeenCalledWith("Stack merged.");
  });

  it("enqueues the first layer on a merge-queue repository and stops there", async () => {
    vi.mocked(toast.success).mockClear();
    const runAction = vi.fn(
      async (_body: CodeDeliveryPullRequestActionBody) => ({
        success: true,
        message: "Added to the merge queue.",
      }),
    );
    await renderStackPanel(
      2302,
      client({ runCodeDeliveryPullRequestAction: runAction }),
      true,
    );

    await confirmStackMerge();

    // The queue entry hands the layer to the host without landing it, so the
    // chain stops and never claims the stack merged.
    expect(runAction).toHaveBeenCalledTimes(1);
    expect(runAction.mock.calls[0]![0]).toMatchObject({
      target: { number: 2301 },
      action: { type: "merge", auto: true },
    });
    expect(toast.success).toHaveBeenCalledWith(
      expect.stringMatching(/#2301 was added to the merge queue/),
    );
    expect(toast.success).not.toHaveBeenCalledWith("Stack merged.");
  });
});

describe("create stack", () => {
  async function renderUnregisteredPanel(number: number, api = client()) {
    const summary = unregisteredDeliveryPullRequests.find(
      (item) => item.number === number,
    );
    if (!summary) throw new Error(`no unregistered fixture for #${number}`);
    render(
      <PullRequestDetailSheet
        client={api}
        summary={summary}
        onClose={vi.fn()}
        onChanged={vi.fn()}
        onOpenWorkspace={vi.fn()}
      />,
    );
    await screen.findByRole("tab", { name: /Conversation/ });
  }

  it("offers to register the chain and posts it bottom to top", async () => {
    const runAction = vi.fn(
      async (_body: CodeDeliveryPullRequestActionBody) => ({
        success: true,
        message: "Registered a stack of 3 pull requests on GitHub",
      }),
    );
    await renderUnregisteredPanel(
      2311,
      client({ runCodeDeliveryPullRequestAction: runAction }),
    );

    await userEvent.click(
      screen.getByRole("button", { name: /Create stack \(3 layers\)/ }),
    );
    await userEvent.click(screen.getByRole("button", { name: "Create stack" }));

    expect(runAction).toHaveBeenCalledTimes(1);
    expect(runAction.mock.calls[0]![0]).toMatchObject({
      target: { number: 2311 },
      action: { type: "create_stack", numbers: [2310, 2311, 2312] },
    });
  });

  it("keeps the single-layer merge beside the offer", async () => {
    await renderUnregisteredPanel(2311);
    expect(
      screen.getByRole("button", { name: /Create stack \(3 layers\)/ }),
    ).toBeInTheDocument();
    // The chain is not registered, so no merge-stack offer exists — but the
    // ordinary merge is still there, and its confirm names the branch below.
    expect(screen.queryByRole("button", { name: /Merge stack/ })).toBeNull();
    expect(screen.getByRole("button", { name: "Merge" })).toBeInTheDocument();
  });
});
