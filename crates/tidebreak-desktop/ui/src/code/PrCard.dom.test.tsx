// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { CodeWorkspacePrSnapshot } from "../api/types";
import { PrCardView } from "./PrCard";

afterEach(() => {
  cleanup();
});

const hostMocks = vi.hoisted(() => ({ openExternal: vi.fn() }));
vi.mock("@/host", () => ({ openExternal: hostMocks.openExternal }));

const BASE: CodeWorkspacePrSnapshot = {
  dirty: false,
  unpushed: false,
  ahead: 0,
  has_upstream: false,
  suggested_commit_message: "first change\n\n1 file changed, 1 insertion(+), 0 deletions(-)",
  gh_found: true,
  gh_authenticated: true,
  remediation: "",
};

function renderState(
  snapshot: CodeWorkspacePrSnapshot,
  extras: Partial<{
    message: string;
    onCommit: () => void;
    onPush: () => void;
    onCreatePr: () => void;
  }> = {},
) {
  const onCommit = extras.onCommit ?? vi.fn();
  const onPush = extras.onPush ?? vi.fn();
  const onCreatePr = extras.onCreatePr ?? vi.fn();
  render(
    <PrCardView
      snapshot={snapshot}
      message={extras.message ?? snapshot.suggested_commit_message}
      busy={null}
      onMessageChange={vi.fn()}
      onCommit={onCommit}
      onPush={onPush}
      onCreatePr={onCreatePr}
    />,
  );
  return { onCommit, onPush, onCreatePr };
}

describe("PrCard", () => {
  it("shows the no-commits state", () => {
    renderState(BASE);
    expect(screen.getByText("No commits")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Commit" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Push" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Create PR" })).toBeDisabled();
  });

  it("enables commit when the tree is dirty", async () => {
    const { onCommit } = renderState({ ...BASE, dirty: true });
    expect(screen.getByText("Uncommitted")).toBeInTheDocument();
    const commit = screen.getByRole("button", { name: "Commit" });
    expect(commit).toBeEnabled();
    await userEvent.setup().click(commit);
    expect(onCommit).toHaveBeenCalledOnce();
  });

  it("enables push when commits are unpushed", async () => {
    const { onPush } = renderState({ ...BASE, unpushed: true, ahead: 1 });
    expect(screen.getByText("Unpushed")).toBeInTheDocument();
    const push = screen.getByRole("button", { name: "Push" });
    expect(push).toBeEnabled();
    await userEvent.setup().click(push);
    expect(onPush).toHaveBeenCalledOnce();
    expect(screen.getByRole("button", { name: "Create PR" })).toBeDisabled();
  });

  it("enables create PR after a push with no pull request", async () => {
    const { onCreatePr } = renderState({
      ...BASE,
      ahead: 2,
      has_upstream: true,
    });
    expect(screen.getByText("Pushed")).toBeInTheDocument();
    const create = screen.getByRole("button", { name: "Create PR" });
    expect(create).toBeEnabled();
    await userEvent.setup().click(create);
    expect(onCreatePr).toHaveBeenCalledOnce();
  });

  it("shows PR state and checks chips", () => {
    renderState({
      ...BASE,
      ahead: 2,
      has_upstream: true,
      pr: {
        number: 12,
        url: "https://github.com/example/demo/pull/12",
        state: "open",
        checks_summary: "2 passing, 1 pending, 0 failing",
      },
    });
    expect(screen.getByText("open")).toBeInTheDocument();
    expect(screen.getByText("2 passing, 1 pending, 0 failing")).toBeInTheDocument();
    expect(screen.getByText("#12", { exact: false })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Create PR" })).toBeDisabled();
  });

  it("shows copyable gh-absent remediation", () => {
    renderState({
      ...BASE,
      gh_found: false,
      gh_authenticated: undefined,
      remediation:
        "gh is not installed.\n\n  git push -u origin tidebreak/first-change\n  gh pr create --title 'first change' --body '...'\n",
    });
    expect(screen.getAllByText(/gh is not installed/).length).toBeGreaterThan(0);
    expect(screen.getByRole("button", { name: "Create PR" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Copy instructions" })).toBeInTheDocument();
  });
});
