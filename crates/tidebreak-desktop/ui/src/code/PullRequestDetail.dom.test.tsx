// @vitest-environment jsdom
import {
  cleanup,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type {
  CodeDeliveryPullRequestActionBody,
  CodeDeliveryPullRequestTarget,
} from "../api/types";
import {
  deliveryPullRequestDetails,
  deliveryPullRequests,
} from "../stories/fixtures";
import { PullRequestDetailPanel } from "./PullRequestDetail";

afterEach(cleanup);

vi.mock("@/openInBrowser", () => ({ openInBrowser: vi.fn() }));
vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn() },
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
    <PullRequestDetailPanel
      client={api}
      summary={summaryFor(number)}
      onClose={vi.fn()}
      onChanged={vi.fn()}
      onOpenWorkspace={vi.fn()}
    />,
  );
  await screen.findByRole("tab", { name: /Conversation/ });
}

describe("PullRequestDetailPanel", () => {
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
    expect(screen.getByRole("button", { name: "Merge" })).toBeDisabled();
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
    await userEvent.click(screen.getByRole("tab", { name: /Checks/ }));
    expect(
      await screen.findByText("1 of 2 passed, 1 failed"),
    ).toBeInTheDocument();
    const rows = screen.getAllByRole("button", { name: /desktop \// });
    expect(rows[0]).toHaveTextContent("desktop / storybook");
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

  it("keeps the comment button inert until there is something to say", async () => {
    await renderPanel(2251);
    expect(screen.getByRole("button", { name: "Comment" })).toBeDisabled();
  });

  it("carries labels, reviewers, and the diffstat in the header", async () => {
    await renderPanel(2251);
    const header = screen.getByRole("heading", {
      name: "Build the delivery center",
    }).parentElement!.parentElement!.parentElement!;
    expect(within(header).getByText("desktop")).toBeInTheDocument();
    expect(within(header).getByText("+2140")).toBeInTheDocument();
    expect(within(header).getByText("−83")).toBeInTheDocument();
    expect(within(header).getByText("devon")).toBeInTheDocument();
  });
});
