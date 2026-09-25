// @vitest-environment jsdom
import { cleanup, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ComponentProps } from "react";
import type { TurnActions } from "./MessageActions";
import {
  MessageList,
  TURN_CANCELLED_NOTICE,
  type ChatMessage,
} from "./MessageList";
import { renderWithRouter } from "./test/router";

const noop = () => undefined;

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

function actions(overrides: Partial<TurnActions> = {}): TurnActions {
  return {
    onRegenerate: vi.fn(),
    onEdit: vi.fn(),
    onBranch: vi.fn(),
    retryModels: [
      {
        label: "Anthropic",
        models: [{ key: "anthropic::claude-opus-5", label: "Claude Opus 5" }],
      },
    ],
    currentModelKey: "anthropic::claude-opus-5",
    pending: false,
    ...overrides,
  };
}

const conversation: ChatMessage[] = [
  {
    id: "u1",
    role: "user",
    text: "Plan a coastal walk",
    turnId: "t1",
    createdAt: "2026-09-20T10:00:00Z",
  },
  {
    id: "a1",
    role: "assistant",
    text: "Start at the harbor.",
    sources: [],
    turnId: "t1",
    createdAt: "2026-09-20T10:00:05Z",
  },
  {
    id: "u2",
    role: "user",
    text: "Make it shorter",
    turnId: "t2",
    createdAt: "2026-09-20T10:01:00Z",
  },
  {
    id: "a2",
    role: "assistant",
    text: "Harbor to the lighthouse.",
    sources: [],
    turnId: "t2",
    createdAt: "2026-09-20T10:01:05Z",
  },
];

async function renderList(
  props: Partial<ComponentProps<typeof MessageList>> = {},
) {
  return renderWithRouter(
    <MessageList
      messages={conversation}
      folderAccessRequests={[]}
      nativeHost={false}
      nativeBusy={false}
      resolvingFolderCalls={new Set()}
      folderAccessErrors={{}}
      decidingApprovalCalls={new Set()}
      approvalErrors={{}}
      busy={false}
      scrollRef={{ current: null }}
      onScroll={noop}
      onApproval={noop}
      onFolderAccessDecision={noop}
      onFolderAccessCancel={noop}
      {...props}
    />,
  );
}

describe("message actions", () => {
  it("regenerates only the latest answer, and branches from any answer", async () => {
    const user = userEvent.setup();
    const turnActions = actions();
    await renderList({ turnActions });

    const answers = screen.getAllByRole("article", { name: "Assistant" });
    expect(
      within(answers[0]).queryByRole("button", { name: "Regenerate" }),
    ).toBeNull();
    await user.click(
      within(answers[1]).getByRole("button", { name: "Regenerate" }),
    );
    expect(turnActions.onRegenerate).toHaveBeenCalledWith("t2", undefined);

    await user.click(
      within(answers[0]).getByRole("button", { name: "Branch from here" }),
    );
    expect(turnActions.onBranch).toHaveBeenCalledWith("t1");
  });

  it("offers nothing to rerun while a turn runs", async () => {
    await renderList({ turnActions: actions(), busy: true });
    expect(screen.queryByRole("button", { name: "Regenerate" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Edit" })).toBeNull();
  });

  it("edits the latest message in place and sends the new text", async () => {
    const user = userEvent.setup();
    const turnActions = actions();
    await renderList({ turnActions });

    // Only the message that opens the latest turn is editable.
    expect(screen.getAllByRole("button", { name: "Edit" })).toHaveLength(1);
    await user.click(screen.getByRole("button", { name: "Edit" }));
    const field = screen.getByRole("textbox", { name: "Message" });
    expect(field).toHaveValue("Make it shorter");

    // Escape puts the message back unchanged.
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("textbox", { name: "Message" })).toBeNull();
    expect(turnActions.onEdit).not.toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: "Edit" }));
    await user.clear(screen.getByRole("textbox", { name: "Message" }));
    await user.type(
      screen.getByRole("textbox", { name: "Message" }),
      "Make it a loop{Enter}",
    );
    expect(turnActions.onEdit).toHaveBeenCalledWith("t2", "Make it a loop");
    expect(screen.queryByRole("textbox", { name: "Message" })).toBeNull();
  });

  it("says before sending that an edit of an answer that acted starts a new conversation", async () => {
    const user = userEvent.setup();
    await renderList({
      turnActions: actions(),
      latestSideEffects: {
        turnId: "t2",
        effects: ["files_written", "connected_apps_called"],
      },
    });

    await user.click(screen.getByRole("button", { name: "Edit" }));
    expect(
      screen.getByText(
        "Your edit replaces an answer that wrote files and used connected apps, so it starts a new conversation. This conversation stays as it is.",
      ),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Send in a new conversation" }),
    ).toBeDisabled();
  });

  it("asks before regenerating an answer that acted, then answers in a new conversation", async () => {
    const user = userEvent.setup();
    const turnActions = actions();
    await renderList({
      turnActions,
      latestSideEffects: { turnId: "t2", effects: ["files_written"] },
    });

    await user.click(screen.getByRole("button", { name: "Regenerate" }));
    expect(turnActions.onRegenerate).not.toHaveBeenCalled();
    expect(
      await screen.findByText(
        "This answer wrote files, so answering again starts a new conversation. This conversation stays as it is.",
      ),
    ).toBeInTheDocument();
    await user.click(
      screen.getByRole("menuitem", {
        name: "Regenerate in a new conversation",
      }),
    );
    expect(turnActions.onRegenerate).toHaveBeenCalledWith("t2", undefined);
  });

  it("continues a stopped answer from its notice instead of regenerating it", async () => {
    const onRetryTurn = vi.fn();
    await renderList({
      turnActions: actions(),
      onRetryTurn,
      messages: [
        ...conversation.slice(0, 3),
        {
          id: "p2",
          role: "assistant",
          text: "Harbor to the",
          sources: [],
          turnId: "t2",
          createdAt: "2026-09-20T10:01:03Z",
        },
        { id: "c2", role: "system", text: TURN_CANCELLED_NOTICE, turnId: "t2" },
      ],
    });

    const answers = screen.getAllByRole("article", { name: "Assistant" });
    const stopped = answers[answers.length - 1];
    expect(
      within(stopped).queryByRole("button", { name: "Regenerate" }),
    ).toBeNull();
    expect(
      within(stopped).getByRole("button", { name: "Branch from here" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Try again" }),
    ).toBeInTheDocument();
  });

  it("pages back to an earlier answer without touching the turns after it", async () => {
    const user = userEvent.setup();
    await renderList({
      turnActions: actions(),
      answerVersions: {
        t1: [
          {
            turnId: "t0",
            messages: [
              {
                id: "a0",
                role: "assistant",
                text: "Start at the dunes.",
                sources: [],
                turnId: "t0",
                createdAt: "2026-09-20T09:59:00Z",
              },
            ],
          },
        ],
      },
    });

    const pager = screen.getByRole("group", { name: "Answer versions" });
    expect(within(pager).getByText("2 of 2")).toBeInTheDocument();
    const continues = "The conversation continues from answer 2.";
    expect(screen.queryByText(continues)).toBeNull();
    await user.click(
      within(pager).getByRole("button", { name: "Previous version" }),
    );
    expect(screen.getByText("Start at the dunes.")).toBeInTheDocument();
    expect(screen.queryByText("Start at the harbor.")).toBeNull();
    expect(screen.getByText("Harbor to the lighthouse.")).toBeInTheDocument();
    expect(
      within(screen.getByRole("group", { name: "Answer versions" })).getByText(
        "1 of 2",
      ),
    ).toBeInTheDocument();
    // The answer on screen is not the one the conversation goes on from.
    expect(screen.getByText(continues)).toBeInTheDocument();
  });

  it("answers a stopped turn again from its notice", async () => {
    const user = userEvent.setup();
    const onRetryTurn = vi.fn();
    await renderList({
      turnActions: actions(),
      onRetryTurn,
      messages: [
        ...conversation.slice(0, 3),
        {
          id: "c2",
          role: "system",
          text: TURN_CANCELLED_NOTICE,
          turnId: "t2",
        },
      ],
    });

    await user.click(screen.getByRole("button", { name: "Try again" }));
    expect(onRetryTurn).toHaveBeenCalledWith({ noticeId: "c2", turnId: "t2" });
  });

  it("offers a retry beside provider settings after an authentication failure", async () => {
    await renderList({
      turnActions: actions(),
      onRetryTurn: vi.fn(),
      messages: [
        conversation[0],
        { id: "f1", role: "turn_failure", category: "auth", turnId: "t1" },
      ],
    });

    expect(
      screen.getByRole("button", { name: "Open provider settings" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Try again" }),
    ).toBeInTheDocument();
  });

  it("marks where a branch's copied history ends and links back", async () => {
    const user = userEvent.setup();
    const onOpen = vi.fn();
    await renderList({
      turnActions: actions(),
      branchOrigin: {
        title: "Coastal walks",
        branchedAt: "2026-09-20T10:00:30Z",
        onOpen,
      },
    });

    const notice = screen.getByRole("note");
    expect(notice).toHaveTextContent("Branched from Coastal walks");
    await user.click(
      within(notice).getByRole("button", { name: "Coastal walks" }),
    );
    expect(onOpen).toHaveBeenCalled();
  });
});
