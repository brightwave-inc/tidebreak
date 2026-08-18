// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type {
  CodeWorkspacePrSnapshot,
  SequencedCodeEventFrame,
} from "../api/types";
import { PrCard, PrCardView } from "./PrCard";
import { resetCodeSessionRegistry } from "./CodeSessionRegistry";
import { useCodeContentRevision } from "./useLiveContent";

afterEach(() => {
  cleanup();
  resetCodeSessionRegistry();
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

  it("reloads git state when the session journal completes a turn", async () => {
    // The regression: the card fetched once on mount, so a turn that dirtied
    // the worktree left "No commits" on screen with Commit disabled until the
    // whole page was reloaded.
    const frames: Array<(frame: SequencedCodeEventFrame) => void> = [];
    const client = {
      getCodeWorkspacePr: vi
        .fn()
        .mockResolvedValueOnce(BASE)
        .mockResolvedValue({ ...BASE, dirty: true }),
      commitCodeWorkspace: vi.fn(),
      pushCodeWorkspace: vi.fn(),
      createCodePullRequest: vi.fn(),
      listCodeSessionTurns: vi.fn().mockResolvedValue([]),
      openCodeEvents: vi.fn(
        (
          _sessionId: string,
          _after: number,
          onFrame: (frame: SequencedCodeEventFrame) => void,
        ) => {
          frames.push(onFrame);
          return { close: vi.fn() } as unknown as WebSocket;
        },
      ),
    };

    function Harness() {
      const revision = useCodeContentRevision("session-1", client);
      return (
        <PrCard client={client} workspaceId="ws-1" contentRevision={revision} />
      );
    }

    render(<Harness />);
    expect(await screen.findByText("No commits")).toBeInTheDocument();
    await waitFor(() => expect(frames).toHaveLength(1));

    act(() => {
      frames[0]?.({
        seq: 1,
        event: {
          type: "turn_completed",
          usage: {
            input_tokens: 1,
            output_tokens: 1,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
          },
        },
      });
    });

    expect(await screen.findByText("Uncommitted")).toBeInTheDocument();
    expect(client.getCodeWorkspacePr).toHaveBeenCalledTimes(2);
    expect(screen.getByRole("button", { name: "Commit" })).toBeEnabled();
  });
});
