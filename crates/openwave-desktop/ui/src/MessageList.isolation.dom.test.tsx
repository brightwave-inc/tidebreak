// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import type { ComponentProps } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MessageList, type ChatMessage } from "./MessageList";

// A tool result view that cannot render. Standing in for the real failure —
// a defensive parser meeting a shape it did not expect — because the shapes
// this build rejects are exactly the ones it renders safely today.
vi.mock("./McpAppCard", () => ({
  McpAppCard: () => {
    throw new Error("malformed mcp app result");
  },
}));

// A pending folder-access prompt that cannot render, for the same reason.
vi.mock("./FolderAccessCard", () => ({
  FolderAccessCard: () => {
    throw new Error("malformed folder access request");
  },
}));

const noop = () => undefined;

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

function list(
  messages: ChatMessage[],
  props: Partial<ComponentProps<typeof MessageList>> = {},
) {
  return render(
    <MessageList
      messages={messages}
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

describe("a tool result that cannot render", () => {
  it("costs its own card and leaves the approval prompt standing", () => {
    // React logs caught errors loudly; keep the test output readable.
    vi.spyOn(console, "error").mockImplementation(() => {});
    list([
      {
        id: "t1",
        role: "tool",
        callId: "c1",
        name: "other",
        status: "completed",
        result: {
          tool: "mcp_app",
          server: "gateway",
          resourceUri: "ui://gateway/app.html",
        },
      },
      {
        id: "t2",
        role: "tool",
        callId: "c2",
        name: "exec",
        status: "waiting_approval",
        preview: { tool: "exec", command: "cargo", args: ["build"], cwd: "." },
      },
      {
        id: "a1",
        role: "approval",
        callId: "c2",
        summary: "Allow OpenWave to run a command?",
        preview: { tool: "exec", command: "cargo", args: ["build"], cwd: "." },
        canApprove: true,
        canRemember: true,
      },
    ]);

    // The broken card says so rather than leaving a gap...
    expect(screen.getByText("This step could not be displayed.")).toBeTruthy();
    // ...and the decision the turn is parked on is still there to make. A
    // phase-wide boundary took this card down with its sibling, which leaves
    // the reader unable to answer and unable to see why.
    expect(
      screen.getByRole("option", { name: /Yes, run it once/ }),
    ).toBeTruthy();
  });
});

describe("a continuation card that cannot render", () => {
  it("leaves the question the turn is waiting on standing", () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    list([], {
      folderAccessRequests: [
        {
          callId: "f1",
          turnId: "turn-1",
          reason: "read the project folder",
          folderHint: null,
          claimedByDesktop: true,
        },
      ],
      userQuestionRequests: [
        {
          callId: "q1",
          turnId: "turn-1",
          questions: [
            {
              id: "q",
              header: "Environment",
              question: "Which environment?",
              options: [
                { id: "o1", label: "Staging", description: "the shared one" },
              ],
              allowFreeForm: false,
            },
          ],
          askedAt: "2026-07-30T00:00:00Z",
        },
      ],
    });

    // Both cards are prompts the turn cannot get past. Sharing one boundary
    // with the transcript meant either one taking the whole surface down.
    expect(screen.getAllByText("This step could not be displayed.").length).toBe(
      1,
    );
    expect(screen.getByText("Which environment?")).toBeTruthy();
  });
});

describe("an entry that cannot be read while the cards are assembled", () => {
  it("costs its own card, not the render that builds them", () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    // Reading this approval throws while its card is being built — during the
    // transcript's own render, which is outside every boundary the cards
    // carry, so it used to reach the app boundary and blank the window.
    const unreadable = {
      id: "a1",
      role: "approval",
      callId: "c1",
      summary: "Allow OpenWave to run a command?",
      preview: { tool: "exec", command: "cargo", args: ["build"], cwd: "." },
      canRemember: true,
      get canApprove(): never {
        throw new Error("unreadable approval projection");
      },
    } as unknown as ChatMessage;

    list([
      unreadable,
      {
        id: "t2",
        role: "tool",
        callId: "c2",
        name: "exec",
        status: "completed",
        preview: { tool: "exec", command: "cargo", args: ["test"], cwd: "." },
      },
    ]);

    // The card that could not be built says so, and the phase's other card is
    // built and rendered as usual.
    expect(screen.getAllByText("This step could not be displayed.").length).toBe(
      1,
    );
    expect(screen.getByText(/cargo test/)).toBeTruthy();
  });
});
