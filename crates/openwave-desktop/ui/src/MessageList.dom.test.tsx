// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ApprovalCard } from "./ApprovalCard";
import { AppContextProvider, type AppContextValue } from "./AppContext";
import type { ApiClient } from "./api";
import { MessageBubble, MessageList, type ChatMessage } from "./MessageList";
import { SourceNavProvider } from "./panel/SourceNav";
import { renderWithRouter } from "./test/router";

const noop = () => undefined;

function card(overrides: Partial<Parameters<typeof ApprovalCard>[0]> = {}) {
  return (
    <ApprovalCard
      callId="call-1"
      summary="Search a site"
      preview={null}
      canApprove
      canRemember
      deciding={false}
      onDecide={noop}
      {...overrides}
    />
  );
}

const ONCE = "1.Yes, allow it once";
const REMEMBER = "2.Yes, and don't ask again in this chat";

/**
 * A grant made in a project chat reaches every chat in it, so the widest rung
 * has to say so — a label that says "this chat" while the server writes
 * something wider is the failure the ladder exists to prevent.
 */
const REMEMBER_IN_PROJECT = "2.Yes, and don't ask again in this project";
const MORE = "More options";

afterEach(cleanup);

describe("approval card interactions", () => {
  it("propagates each decision with the grant it names", async () => {
    const user = userEvent.setup();
    const onDecide = vi.fn();
    render(card({ onDecide }));

    const options = screen.getAllByRole("option");
    expect(options.map((option) => option.textContent)).toEqual([
      ONCE,
      REMEMBER,
      "3.No, don't allow this",
    ]);

    await user.click(options[0]!);
    expect(onDecide).toHaveBeenLastCalledWith("call-1", "approve", null);

    await user.click(options[1]!);
    expect(onDecide).toHaveBeenLastCalledWith("call-1", "approve", "whole_tool");

    await user.click(options[2]!);
    expect(onDecide).toHaveBeenLastCalledWith("call-1", "reject", null);
  });

  it("starts on the narrowest grant so a stray Enter cannot widen scope", () => {
    render(card());

    const options = screen.getAllByRole("option");
    expect(options[0]).toHaveAttribute("aria-selected", "true");
    expect(options[0]?.textContent).toBe(ONCE);
  });

  it("arms its keyboard shortcuts without needing a click first", () => {
    render(card());

    expect(document.activeElement).toBe(screen.getAllByRole("option")[0]);
  });

  it("leaves focus alone when the user is typing elsewhere", () => {
    const composer = document.createElement("textarea");
    document.body.append(composer);
    composer.focus();

    render(card());

    expect(document.activeElement).toBe(composer);
    composer.remove();
  });

  it("submits the highlighted option from the keyboard", async () => {
    const user = userEvent.setup();
    const onDecide = vi.fn();
    render(card({ onDecide }));

    await user.keyboard("{ArrowDown}{Enter}");
    expect(onDecide).toHaveBeenLastCalledWith("call-1", "approve", "whole_tool");

    await user.keyboard("3{Enter}");
    expect(onDecide).toHaveBeenLastCalledWith("call-1", "reject", null);
  });

  it("wraps around the ends of the list", async () => {
    const user = userEvent.setup();
    const onDecide = vi.fn();
    render(card({ onDecide }));

    await user.keyboard("{ArrowUp}{Enter}");
    expect(onDecide).toHaveBeenLastCalledWith("call-1", "reject", null);
  });

  it("blocks every decision while one is in flight", async () => {
    const user = userEvent.setup();
    const onDecide = vi.fn();
    render(card({ onDecide, deciding: true }));

    for (const option of screen.getAllByRole("option")) {
      await user.click(option);
    }
    expect(screen.getByRole("button", { name: "Submit" })).toBeDisabled();
    expect(onDecide).not.toHaveBeenCalled();
  });

  it("announces a failed decision and stays actionable for retry", async () => {
    const user = userEvent.setup();
    const onDecide = vi.fn();
    render(
      card({ onDecide, error: "Could not send your decision: Error: 500" }),
    );

    expect(screen.getByRole("alert")).toHaveTextContent(
      "Could not send your decision",
    );
    await user.click(screen.getAllByRole("option")[0]!);
    expect(onDecide).toHaveBeenCalledWith("call-1", "approve", null);
  });

  it("offers only rejection when the action kind is not approvable", () => {
    render(card({ canApprove: false }));

    expect(
      screen.getAllByRole("option").map((option) => option.textContent),
    ).toEqual(["1.No, don't allow this"]);
  });

  it("offers one-shot approval but no remembered grant for MCP", () => {
    render(card({ canRemember: false }));

    expect(
      screen.getAllByRole("option").map((option) => option.textContent),
    ).toEqual([ONCE, "2.No, don't allow this"]);
  });

  it("hides the broader grants behind one more keystroke", async () => {
    const user = userEvent.setup();
    const onDecide = vi.fn();
    render(
      card({
        onDecide,
        preview: {
          tool: "exec",
          command: "cargo",
          args: ["test"],
          cwd: ".",
        },
        // Both prefix rungs the server offers for `cargo test`.
        prefixRungs: [2, 1],
      }),
    );

    // Every option on screen is one keystroke away, so the narrowest grant is
    // inline and the wider ones are not.
    expect(
      screen.getAllByRole("option").map((option) => option.textContent),
    ).toEqual([
      "1.Yes, run it once",
      "2.Yes, and always allow exactly \u201ccargo test\u201d",
      `3.${MORE}`,
      "4.No, don't allow this",
    ]);

    await user.click(screen.getByText(MORE));
    expect(
      screen.getAllByRole("option").map((option) => option.textContent),
    ).toEqual([
      "1.Yes, run it once",
      "2.Yes, and always allow exactly \u201ccargo test\u201d",
      // The rung this ladder previously could not offer.
      "3.Yes, and always allow any \u201ccargo test\u201d command",
      "4.Yes, and always allow any \u201ccargo\u201d command",
      "5.Yes, and don't ask again about commands in this chat",
      "6.No, don't allow this",
    ]);
    expect(onDecide).not.toHaveBeenCalled();
  });

  it("names the project when a remembered answer will reach past this chat", async () => {
    render(card({ grantScope: "project" }));

    expect(
      screen.getAllByRole("option").map((row) => row.textContent),
    ).toEqual([ONCE, REMEMBER_IN_PROJECT, "3.No, don't allow this"]);
    // And says once, in full, what the rows cannot say without becoming
    // three long lines.
    screen.getByText(/Saved answers apply to every chat in this project/);
  });

  it("returns the highlight to the narrowest grant when it widens the list", async () => {
    const user = userEvent.setup();
    const onDecide = vi.fn();
    render(
      card({
        onDecide,
        preview: { tool: "exec", command: "cargo", args: ["test"], cwd: "." },
      }),
    );

    // "More options" sat at row 3; expanding puts a broader grant there. A
    // stray Enter must not commit whatever moved under the cursor.
    await user.keyboard("3{Enter}");
    expect(screen.getAllByRole("option")[0]).toHaveAttribute(
      "aria-selected",
      "true",
    );
    await user.keyboard("{Enter}");
    expect(onDecide).toHaveBeenCalledWith("call-1", "approve", null);
  });

  it("offers a search the query it showed, not every search in the chat", async () => {
    const user = userEvent.setup();
    const onDecide = vi.fn();
    render(
      card({
        onDecide,
        summary:
          "Allow web search to send this query and its explicit filters to the configured search provider outside OpenWave?",
        preview: {
          tool: "web_search",
          query: "quarterly filings",
          domains: ["sec.gov"],
          start_published_at: null,
          end_published_at: null,
        },
      }),
    );

    // The ladder used to be exec-shaped, so this card's only standing option
    // was the widest rung there is.
    expect(
      screen.getAllByRole("option").map((option) => option.textContent),
    ).toEqual([
      ONCE,
      "2.Yes, and always allow exactly “quarterly filings”",
      `3.${MORE}`,
      "4.No, don't allow this",
    ]);
    // The filters are part of what is being consented to, so the card shows
    // them alongside the query it leads with.
    expect(screen.getByText(/# limited to sec.gov/)).toBeInTheDocument();

    await user.click(screen.getAllByRole("option")[1]!);
    expect(onDecide).toHaveBeenLastCalledWith(
      "call-1",
      "approve",
      "exact_action",
    );
  });

  it("keeps a long query to one row of the option list", () => {
    render(
      card({
        preview: {
          tool: "web_search",
          query:
            "what did the company say about revenue recognition in the most recent quarter",
          domains: [],
          start_published_at: null,
          end_published_at: null,
        },
      }),
    );

    // A natural-language query runs to hundreds of characters; the unabridged
    // one is in the block above, which is where it is meant to be read.
    expect(screen.getAllByRole("option")[1]?.textContent).toBe(
      "2.Yes, and always allow exactly “what did the company say about revenue recogniti…”",
    );
  });

  it("offers only the whole tool when the action names nothing narrower", async () => {
    const user = userEvent.setup();
    render(card());

    expect(screen.queryByText(MORE)).not.toBeInTheDocument();
    expect(
      screen.getAllByRole("option").map((option) => option.textContent),
    ).toEqual([ONCE, REMEMBER, "3.No, don't allow this"]);
    await user.click(screen.getAllByRole("option")[1]!);
  });

  it("asks about the command rather than about commands in general", () => {
    render(
      card({
        summary:
          "Allow OpenWave to run a command that leaves the chat workspace and may reach the network?",
        preview: {
          tool: "exec",
          command: "cargo",
          args: ["test", "--workspace"],
          cwd: "checkout",
        },
      }),
    );

    expect(
      screen.getByRole("heading", { name: "Run this command?" }),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/cargo test --workspace/, { selector: "pre" }),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/# working directory: checkout/),
    ).toBeInTheDocument();
    // The class-of-egress sentence becomes the subheading, not the ask.
    expect(screen.getByText(/may reach the network/)).toBeInTheDocument();
    expect(screen.getAllByRole("option")[0]).toHaveTextContent(
      "Yes, run it once",
    );
  });
});

describe("activity phases", () => {
  const preview = {
    tool: "exec" as const,
    command: "cargo",
    args: ["build"],
    cwd: ".",
  };

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

  it("keeps a run of calls behind one line and reveals them on demand", async () => {
    const user = userEvent.setup();
    list([
      { id: "t1", role: "tool", callId: "c1", name: "search", status: "completed" },
      { id: "t2", role: "tool", callId: "c2", name: "read_file", status: "completed" },
      { id: "t3", role: "tool", callId: "c3", name: "web_search", status: "completed" },
    ]);

    const trigger = screen.getByRole("button", {
      name: /Searched the web and 2 other tasks/,
    });
    expect(screen.queryByRole("listitem")).not.toBeInTheDocument();

    await user.click(trigger);
    expect(
      screen.getAllByRole("listitem").map((row) => row.textContent),
    ).toEqual(["Searched sources", "Read a file", "Searched the web"]);
  });

  it("surfaces a command card outside the collapsed region", () => {
    list([
      { id: "t1", role: "tool", callId: "c1", name: "search", status: "completed" },
      {
        id: "t2",
        role: "tool",
        callId: "c2",
        name: "exec",
        status: "completed",
        preview,
      },
    ]);

    // Collapsed: the rail lists nothing, but the command is still readable.
    expect(screen.queryByRole("listitem")).not.toBeInTheDocument();
    expect(screen.getByText("cargo build")).toBeInTheDocument();
    expect(screen.getByText("Done")).toBeInTheDocument();
  });

  it("lets the approval card speak for a call parked on it", () => {
    list([
      {
        id: "t1",
        role: "tool",
        callId: "c1",
        name: "exec",
        status: "waiting_approval",
        preview,
      },
      {
        id: "a1",
        role: "approval",
        callId: "c1",
        summary: "Allow OpenWave to run a command?",
        preview,
        canApprove: true,
        canRemember: true,
      },
    ]);

    // One copy of the command, on the card that can act on it, and no rail
    // line announcing the same pending action a second time.
    expect(screen.getAllByText(/cargo build/, { selector: "pre" })).toHaveLength(
      1,
    );
    expect(
      screen.queryByRole("button", { name: /Running a command/ }),
    ).toBeNull();
  });
});

describe("historical image attachments", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("fetches image pixels with the chat client and releases the object URL", async () => {
    const createObjectUrl = vi.fn(() => "blob:transcript-image");
    const revokeObjectUrl = vi.fn();
    vi.stubGlobal("URL", {
      createObjectURL: createObjectUrl,
      revokeObjectURL: revokeObjectUrl,
    });
    const getChatImageAttachment = vi.fn(async () =>
      new Blob(["pixels"], { type: "image/png" }),
    );
    const { unmount } = render(
      <MessageBubble
        message={{
          id: "user-image",
          role: "user",
          text: "Describe this",
          images: [
            {
              attachmentId: "image-opaque-id",
              mediaType: "image/png",
              width: 320,
              height: 240,
            },
          ],
        }}
        busy={false}
        imageClient={{ getChatImageAttachment }}
        chatId="chat-1"
      />,
    );

    await waitFor(() =>
      expect(getChatImageAttachment).toHaveBeenCalledWith(
        "chat-1",
        "image-opaque-id",
        expect.any(AbortSignal),
      ),
    );
    const image = await screen.findByRole("img", {
      name: "Attached image 1: 320 by 240 pixels",
    });
    expect(image).toHaveAttribute("src", "blob:transcript-image");
    expect(createObjectUrl).toHaveBeenCalledTimes(1);

    unmount();
    expect(revokeObjectUrl).toHaveBeenCalledWith("blob:transcript-image");
  });
});

describe("card order", () => {
  it("follows the order the calls happened, not the kind of card", () => {
    const preview = {
      tool: "exec" as const,
      command: "cargo",
      args: ["build"],
      cwd: ".",
    };
    const { container } = render(
      <MessageList
        messages={[
          { id: "t1", role: "tool", callId: "c1", name: "exec", status: "completed", preview },
          {
            id: "a1",
            role: "approval",
            callId: "c2",
            summary: "Allow web search?",
            preview: null,
            canApprove: true,
            canRemember: true,
          },
          { id: "t2", role: "tool", callId: "c2", name: "web_search", status: "waiting_approval" },
        ]}
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

    const cards = Array.from(
      container.querySelectorAll("section[aria-label]"),
    ).map((card) => card.getAttribute("aria-label"));
    expect(cards).toEqual([
      "Run a command: Command complete",
      "Approval needed",
    ]);
  });
});

describe("command output", () => {
  const preview = {
    tool: "exec" as const,
    command: "cargo",
    args: ["build"],
    cwd: ".",
  };
  const ran = {
    tool: "exec" as const,
    exitCode: 0,
    timedOut: false,
    outputTruncated: false,
    stdout: "",
    stderr: "",
  };

  function list(result: typeof ran | null) {
    return render(
      <MessageList
        messages={[
          {
            id: "t1",
            role: "tool",
            callId: "c1",
            name: "exec",
            status: "completed",
            preview,
            result,
          },
        ]}
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

  it("opens onto the output, with the command one tab away", async () => {
    const user = userEvent.setup();
    list({ ...ran, stdout: "two tests passed\n" });

    await user.click(screen.getByRole("button", { name: /cargo build/ }));
    expect(screen.getByText(/two tests passed/)).toBeInTheDocument();

    await user.click(screen.getByRole("tab", { name: "command" }));
    expect(screen.getByText(/cargo build/, { selector: "pre" })).toBeInTheDocument();
  });

  it("drops the tab pair for a command that finished silently", async () => {
    const user = userEvent.setup();
    list(ran);

    await user.click(screen.getByRole("button", { name: /cargo build/ }));
    expect(screen.queryByRole("tab")).not.toBeInTheDocument();
    expect(screen.getByText(/cargo build/, { selector: "pre" })).toBeInTheDocument();
  });
});

describe("mcp app views", () => {
  it("surfaces a sandboxed app card for an mcp_app result", async () => {
    const client = {
      baseUrl: "http://127.0.0.1:7777",
      createMcpViewFrame: vi
        .fn()
        .mockResolvedValue({ frame_path: "/mcp/view-frames/token-1" }),
    };
    render(
      <AppContextProvider
        value={{ client: client as unknown as ApiClient } as AppContextValue}
      >
        <MessageList
          messages={[
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
          ]}
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
        />
      </AppContextProvider>,
    );

    expect(
      await screen.findByTitle("MCP App view from gateway"),
    ).toBeInTheDocument();
  });

  it("surfaces what a call found, behind one line until it is opened", async () => {
    const user = userEvent.setup();
    render(
      <MessageList
        messages={[
          {
            id: "t1",
            role: "tool",
            callId: "c1",
            name: "list_dir",
            status: "completed",
            result: {
              tool: "entries",
              elided: 3,
              entries: [
                { kind: "file", label: "notes.md", detail: null, meta: "1.2 KB", mediaType: null },
                { kind: "folder", label: "reports", detail: null, meta: null, mediaType: null },
              ],
              failures: [],
            },
          },
        ]}
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

    // Collapsed, the phase is one line: no standing card, no rows.
    expect(screen.queryByText("notes.md")).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /Browsed files/ }));
    // The count line counts the rows the list is not showing — the reader
    // must not be told the call found two things when it found five.
    expect(screen.getByText("Found 5 items")).toBeInTheDocument();
    expect(screen.getByText("notes.md")).toBeInTheDocument();
    expect(screen.getByText("reports")).toBeInTheDocument();
    expect(screen.getByText("3 more not shown")).toBeInTheDocument();
  });

  it("shows a search as its query over the documents it matched", async () => {
    const user = userEvent.setup();
    render(
      <MessageList
        messages={[
          {
            id: "t1",
            role: "tool",
            callId: "c1",
            name: "search",
            status: "completed",
            preview: { tool: "search", query: "quarterly revenue" },
            result: {
              tool: "entries",
              elided: 0,
              entries: [
                {
                  kind: "source",
                  label: "Q3 Report",
                  detail: "Pages 3, 7",
                  meta: "2 matches",
                  mediaType: "application/pdf",
                },
              ],
              failures: [],
            },
          },
        ]}
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

    await user.click(screen.getByRole("button", { name: /Searched sources/ }));
    // The row reads as the title over the query — what ran and what it was
    // about — over the documents it matched, named and counted.
    expect(screen.getByText("quarterly revenue")).toBeInTheDocument();
    expect(screen.getByText("Searched 1 source")).toBeInTheDocument();
    expect(screen.getByText("Q3 Report")).toBeInTheDocument();
    expect(screen.getByText("2 matches")).toBeInTheDocument();
  });

  it("says on the collapsed header that some of the call failed", async () => {
    const user = userEvent.setup();
    render(
      <MessageList
        messages={[
          {
            id: "t1",
            role: "tool",
            callId: "c1",
            name: "read_file",
            status: "completed",
            result: {
              tool: "entries",
              elided: 0,
              entries: [
                { kind: "file", label: "q3.md", detail: null, meta: null, mediaType: null },
              ],
              failures: [
                { label: "q4.md", error: "file is not valid UTF-8" },
                { label: null, error: "the folder is no longer available" },
              ],
            },
          },
        ]}
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

    // A partial success is not a clean one: the failures render in their own
    // divided section of the same list, not silently dropped.
    await user.click(screen.getByRole("button", { name: /Read a file/ }));
    expect(screen.getByText("q3.md")).toBeInTheDocument();
    expect(screen.getByText("file is not valid UTF-8")).toBeInTheDocument();
    // A failure the tool could not name still gets a row rather than vanishing.
    expect(screen.getByText("the folder is no longer available")).toBeInTheDocument();
    expect(screen.getByText("Item")).toBeInTheDocument();
  });
});

describe("actionable tool results", () => {
  it("takes an unconfigured web search directly to its settings section", async () => {
    const user = userEvent.setup();
    const { router } = await renderWithRouter(
      <MessageList
        messages={[
          {
            id: "web-search-1",
            role: "tool",
            callId: "call-1",
            name: "web_search",
            status: "failed",
            result: { tool: "web_search_provider_required" },
          },
        ]}
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

    expect(
      screen.getByRole("heading", { name: "Web search needs a provider" }),
    ).toBeInTheDocument();
    await user.click(
      screen.getByRole("button", { name: "Configure web search" }),
    );
    await waitFor(() => {
      expect(router.state.location.pathname).toBe("/settings/web-search");
    });
  });
});

describe("citation anchors", () => {
  const CITATION = "citation-1";
  const DOCUMENT = "0b2b1f2c-9d3e-4a5b-8c7d-6e5f4a3b2c1d";

  const message: ChatMessage = {
    id: "assistant-1",
    role: "assistant",
    text: `The reef :cit[is the largest in the world]{doc=${DOCUMENT} page=2}.`,
    sources: [
      {
        id: CITATION,
        ordinal: 1,
        documentId: DOCUMENT,
        locator: { kind: "page", page: 2 },
      },
    ],
  };

  it("opens the same place from the phrase and from the sources row", async () => {
    const user = userEvent.setup();
    const openCitation = vi.fn();
    render(
      <SourceNavProvider value={{ openCitation, openDocument: vi.fn() }}>
        <MessageBubble message={message} busy={false} />
      </SourceNavProvider>,
    );

    await user.click(screen.getByRole("button", { name: /citation 1$/ }));
    await user.click(screen.getByRole("button", { name: "Open source 1" }));

    expect(openCitation).toHaveBeenCalledTimes(2);
    expect(openCitation.mock.calls[0]).toEqual(openCitation.mock.calls[1]);
    expect(openCitation).toHaveBeenLastCalledWith({
      documentId: DOCUMENT,
      citationId: CITATION,
    });
  });

  it("copies what the message reads as, not how a citation is stored", async () => {
    const user = userEvent.setup();
    render(<MessageBubble message={message} busy={false} />);

    await user.click(screen.getByRole("button", { name: "Copy" }));
    expect(await window.navigator.clipboard.readText()).toBe(
      "The reef is the largest in the world.",
    );
  });
});

describe("file attachment chips", () => {
  it("open the attached document in the viewer panel", async () => {
    const user = userEvent.setup();
    const openDocument = vi.fn();
    render(
      <SourceNavProvider
        value={{ openCitation: vi.fn(), openDocument }}
      >
        <MessageBubble
          busy={false}
          message={{
            id: "user-file",
            role: "user",
            text: "Read this",
            files: [
              {
                documentId: "document-1",
                name: "brief.pdf",
                mediaType: "application/pdf",
              },
            ],
          }}
        />
      </SourceNavProvider>,
    );

    await user.click(screen.getByRole("button", { name: "brief.pdf" }));
    expect(openDocument).toHaveBeenCalledWith("document-1");
  });
});
