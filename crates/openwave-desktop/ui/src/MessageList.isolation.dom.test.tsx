// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
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

const noop = () => undefined;

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

function list(messages: ChatMessage[]) {
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
