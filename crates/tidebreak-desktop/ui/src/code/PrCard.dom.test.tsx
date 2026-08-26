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
  suggested_commit_message:
    "first change\n\n1 file changed, 1 insertion(+), 0 deletions(-)",
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
    expect(screen.getByText(/No local commits yet/)).toBeInTheDocument();
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
  });

  it("enables commit when the tree is dirty", async () => {
    const { onCommit } = renderState({ ...BASE, dirty: true });
    expect(screen.getByText("Uncommitted")).toBeInTheDocument();
    const commit = screen.getByRole("button", { name: "Commit changes" });
    expect(commit).toBeEnabled();
    await userEvent.setup().click(commit);
    expect(onCommit).toHaveBeenCalledOnce();
    expect(
      screen.queryByRole("button", { name: "Push branch" }),
    ).not.toBeInTheDocument();
  });

  it("enables push when commits are unpushed", async () => {
    const { onPush } = renderState({ ...BASE, unpushed: true, ahead: 1 });
    expect(screen.getByText("Unpushed")).toBeInTheDocument();
    const push = screen.getByRole("button", { name: "Push branch" });
    expect(push).toBeEnabled();
    await userEvent.setup().click(push);
    expect(onPush).toHaveBeenCalledOnce();
    expect(
      screen.queryByRole("button", { name: "Create pull request" }),
    ).not.toBeInTheDocument();
  });

  it("states plainly when pushes land as the deployment's GitHub App", () => {
    renderState({
      ...BASE,
      unpushed: true,
      ahead: 1,
      gh_found: false,
      gh_authenticated: undefined,
      pushes_as: "tidebreak-ship[bot]",
    });
    expect(screen.getByText("tidebreak-ship[bot]")).toBeInTheDocument();
    expect(screen.getByText(/not as your GitHub account/)).toBeInTheDocument();
  });

  it("names the caller when pushes land as their own account", () => {
    renderState({
      ...BASE,
      unpushed: true,
      ahead: 1,
      gh_found: false,
      gh_authenticated: undefined,
      pushes_as: "mira-chen",
      pushes_as_self: true,
    });
    expect(screen.getByText("mira-chen")).toBeInTheDocument();
    expect(screen.getByText(/your own GitHub account/)).toBeInTheDocument();
    expect(
      screen.queryByText(/not as your GitHub account/),
    ).not.toBeInTheDocument();
  });

  it("names no acting identity on a machine with its own credentials", () => {
    renderState({ ...BASE, unpushed: true, ahead: 1 });
    expect(
      screen.queryByText(/GitHub App — not as your GitHub account/),
    ).not.toBeInTheDocument();
  });

  it("enables create PR after a push with no pull request", async () => {
    const { onCreatePr } = renderState({
      ...BASE,
      ahead: 2,
      has_upstream: true,
    });
    expect(screen.getByText("Pushed")).toBeInTheDocument();
    const create = screen.getByRole("button", {
      name: "Create pull request",
    });
    expect(create).toBeEnabled();
    await userEvent.setup().click(create);
    expect(onCreatePr).toHaveBeenCalledOnce();
  });

  it("shows a compact link for an existing clean pull request", async () => {
    hostMocks.openExternal.mockResolvedValue(true);
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
    expect(screen.getAllByText("Open").length).toBeGreaterThan(0);
    expect(screen.getByText("Pull request #12")).toBeInTheDocument();
    const open = screen.getByRole("link", { name: "Open" });
    await userEvent.setup().click(open);
    expect(hostMocks.openExternal).toHaveBeenCalledWith(
      "https://github.com/example/demo/pull/12",
    );
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
  });

  it("shows queued instead of open after GitHub accepts the merge queue entry", () => {
    renderState({
      ...BASE,
      pr: {
        number: 12,
        url: "https://github.com/example/demo/pull/12",
        state: "open",
        in_merge_queue: true,
      },
    });

    expect(screen.getByText("In merge queue")).toHaveClass(
      "bg-info-background",
    );
    expect(screen.queryByText("open")).not.toBeInTheDocument();
  });

  it("keeps an existing pull request visible while changes are dirty", () => {
    renderState({
      ...BASE,
      dirty: true,
      pr: {
        number: 12,
        url: "https://github.com/example/demo/pull/12",
        state: "open",
        checks_summary: "2 passing, 1 pending, 0 failing",
      },
    });
    expect(screen.getAllByText("Open").length).toBeGreaterThan(0);
    expect(screen.getByText("Uncommitted")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Commit changes" }),
    ).toBeEnabled();
  });

  it("keeps an existing pull request visible while commits are unpushed", () => {
    renderState({
      ...BASE,
      unpushed: true,
      ahead: 1,
      pr: {
        number: 12,
        url: "https://github.com/example/demo/pull/12",
        state: "open",
        checks_summary: "2 passing, 1 pending, 0 failing",
      },
    });
    expect(screen.getAllByText("Open").length).toBeGreaterThan(0);
    expect(screen.getByText("Unpushed")).toBeInTheDocument();
    expect(screen.getByText(/update #12/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Push branch" })).toBeEnabled();
  });

  it("shows copyable gh-absent remediation for a pushed branch", () => {
    renderState({
      ...BASE,
      ahead: 1,
      has_upstream: true,
      gh_found: false,
      gh_authenticated: undefined,
      remediation:
        "gh is not installed.\n\n  git push -u origin tidebreak/first-change\n  gh pr create --title 'first change' --body '...'\n",
    });
    expect(screen.getAllByText(/gh is not installed/).length).toBeGreaterThan(
      0,
    );
    expect(
      screen.getByRole("button", { name: "Create pull request" }),
    ).toBeDisabled();
    expect(
      screen.getByRole("button", { name: "Copy instructions" }),
    ).toBeInTheDocument();
  });

  it("shows signed-out remediation for a pushed branch", () => {
    renderState({
      ...BASE,
      ahead: 1,
      has_upstream: true,
      gh_authenticated: false,
      remediation: "gh auth login",
    });
    expect(screen.getAllByText(/not signed in/).length).toBeGreaterThan(0);
    expect(
      screen.getByRole("button", { name: "Create pull request" }),
    ).toBeDisabled();
    expect(screen.getByText("gh auth login")).toBeInTheDocument();
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
            context_tokens: 0,
          },
        },
      });
    });

    expect(await screen.findByText("Uncommitted")).toBeInTheDocument();
    expect(client.getCodeWorkspacePr).toHaveBeenCalledTimes(2);
    expect(
      screen.getByRole("button", { name: "Commit changes" }),
    ).toBeEnabled();
  });

  it("refreshes an untouched suggested commit message with the latest diffstat", async () => {
    const first = {
      ...BASE,
      dirty: true,
      suggested_commit_message:
        "Improve source control\n\n2 files changed, 30 insertions(+), 23 deletions(-)",
    };
    const second = {
      ...first,
      suggested_commit_message:
        "Improve source control\n\n1 file changed, 5 insertions(+), 5 deletions(-)",
    };
    const client = {
      getCodeWorkspacePr: vi
        .fn()
        .mockResolvedValueOnce(first)
        .mockResolvedValue(second),
      commitCodeWorkspace: vi.fn(),
      pushCodeWorkspace: vi.fn(),
      createCodePullRequest: vi.fn(),
    };
    const { rerender } = render(
      <PrCard client={client} workspaceId="ws-1" contentRevision={0} />,
    );

    const message = await screen.findByRole("textbox", {
      name: "Commit message",
    });
    await waitFor(() =>
      expect(message).toHaveValue(first.suggested_commit_message),
    );

    rerender(<PrCard client={client} workspaceId="ws-1" contentRevision={1} />);
    await waitFor(() =>
      expect(message).toHaveValue(second.suggested_commit_message),
    );
  });

  it("preserves an edited commit message when git state refreshes", async () => {
    const client = {
      getCodeWorkspacePr: vi
        .fn()
        .mockResolvedValueOnce({ ...BASE, dirty: true })
        .mockResolvedValue({
          ...BASE,
          dirty: true,
          suggested_commit_message: "new generated suggestion",
        }),
      commitCodeWorkspace: vi.fn(),
      pushCodeWorkspace: vi.fn(),
      createCodePullRequest: vi.fn(),
    };
    const { rerender } = render(
      <PrCard client={client} workspaceId="ws-1" contentRevision={0} />,
    );
    const message = await screen.findByRole("textbox", {
      name: "Commit message",
    });
    await waitFor(() =>
      expect(message).toHaveValue(BASE.suggested_commit_message),
    );
    await userEvent.setup().clear(message);
    await userEvent.setup().type(message, "Keep my message");

    rerender(<PrCard client={client} workspaceId="ws-1" contentRevision={1} />);
    await waitFor(() =>
      expect(client.getCodeWorkspacePr).toHaveBeenCalledTimes(2),
    );
    expect(message).toHaveValue("Keep my message");
  });
});
