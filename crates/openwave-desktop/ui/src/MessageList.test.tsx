import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { MessageBubble, MessageList, type ChatMessage } from "./MessageList";

const noop = () => undefined;

describe("MessageBubble", () => {
  it("keeps the user compact while rendering assistant Markdown without a bubble", () => {
    const user = renderToStaticMarkup(
      <MessageBubble
        message={{ id: "user-1", role: "user", text: "Hello **there**" }}
        busy={false}
        onApproval={noop}
      />,
    );
    const assistant = renderToStaticMarkup(
      <MessageBubble
        message={{
          id: "assistant-1",
          role: "assistant",
          text: "## Answer",
          sources: [],
        }}
        busy={false}
        onApproval={noop}
      />,
    );

    expect(user).toContain('class="message message-user"');
    expect(user).toContain("<strong>there</strong>");
    expect(assistant).toContain('class="message message-assistant"');
    expect(assistant).toContain("<h2>Answer</h2>");
    expect(assistant).not.toContain("bubble");
  });

  it("keeps tool cards, approvals, and active streaming placeholders in sequence", () => {
    const messages: ChatMessage[] = [
      { id: "tool-1", role: "tool", callId: "call-1", name: "web_search", status: "running" },
      {
        id: "approval-1",
        role: "approval",
        callId: "call-1",
        summary: "Search a site",
        canApprove: true,
      },
      { id: "assistant-2", role: "assistant", text: "", sources: [] },
    ];
    const markup = renderToStaticMarkup(
      <MessageList
        messages={messages}
        folderAccessRequests={[]}
        nativeHost={false}
        nativeBusy={false}
        resolvingFolderCalls={new Set()}
        folderAccessErrors={{}}
        busy
        scrollRef={{ current: null }}
        onScroll={noop}
        onApproval={noop}
        onFolderAccessDecision={noop}
        onFolderAccessCancel={noop}
      />,
    );

    expect(markup.indexOf("Search the web")).toBeLessThan(
      markup.indexOf("Approval needed"),
    );
    expect(markup).toContain("message-approval");
    expect(markup).toContain('aria-label="Thinking"');
  });

  it("does not offer approval for an action without a safe description", () => {
    const markup = renderToStaticMarkup(
      <MessageBubble
        message={{
          id: "approval-unknown",
          role: "approval",
          callId: "call-unknown",
          summary: "The exact action cannot be safely described.",
          canApprove: false,
        }}
        busy
        onApproval={noop}
      />,
    );

    expect(markup).toContain("The exact action cannot be safely described.");
    expect(markup).not.toContain(">Approve<");
    expect(markup).toContain(">Reject<");
  });

  it("collapses only contiguous terminal tool activity using safe card copy", () => {
    const messages: ChatMessage[] = [
      { id: "tool-1", role: "tool", callId: "call-1", name: "web_search", status: "completed" },
      { id: "tool-2", role: "tool", callId: "call-2", name: "read_file", status: "failed" },
      {
        id: "assistant-1",
        role: "assistant",
        text: "Done",
        sources: [],
      },
      { id: "tool-3", role: "tool", callId: "call-3", name: "list_dir", status: "cancelled" },
    ];
    const markup = renderToStaticMarkup(
      <MessageList
        messages={messages}
        folderAccessRequests={[]}
        nativeHost={false}
        nativeBusy={false}
        resolvingFolderCalls={new Set()}
        folderAccessErrors={{}}
        busy={false}
        scrollRef={{ current: null }}
        onScroll={noop}
        onApproval={noop}
        onFolderAccessDecision={noop}
        onFolderAccessCancel={noop}
      />,
    );

    expect(markup).toContain('class="tool-activity-group is-settled"');
    expect(markup).toContain('aria-expanded="false"');
    expect(markup).toContain('aria-controls="tool-activity-group-0"');
    expect(markup).toContain('id="tool-activity-group-0" hidden=""');
    expect(markup).toContain(
      "2 tool activities · 1 completed · 1 failed",
    );
    expect(markup).toContain("Web search complete");
    expect(markup).toContain("Tool could not complete");
    expect(markup.indexOf("Web search complete")).toBeLessThan(
      markup.indexOf("Done"),
    );
    expect(markup.indexOf("Done")).toBeLessThan(markup.indexOf("Not run"));
  });

  it("leaves active and folder-access activity individually visible", () => {
    const messages: ChatMessage[] = [
      { id: "tool-1", role: "tool", callId: "call-1", name: "web_search", status: "completed" },
      { id: "tool-2", role: "tool", callId: "call-2", name: "list_dir", status: "running" },
      { id: "tool-3", role: "tool", callId: "call-3", name: "request_folder_access", status: "completed" },
      { id: "tool-4", role: "tool", callId: "call-4", name: "read_file", status: "completed" },
    ];
    const markup = renderToStaticMarkup(
      <MessageList
        messages={messages}
        folderAccessRequests={[]}
        nativeHost={false}
        nativeBusy={false}
        resolvingFolderCalls={new Set()}
        folderAccessErrors={{}}
        busy={false}
        scrollRef={{ current: null }}
        onScroll={noop}
        onApproval={noop}
        onFolderAccessDecision={noop}
        onFolderAccessCancel={noop}
      />,
    );

    expect(markup).not.toContain("tool-activity-group");
    expect(markup).toContain("Browsing files");
    expect(markup).toContain("Folder access request complete");
  });

  it("does not expose provider tool names in an activity summary", () => {
    const messages: ChatMessage[] = [
      {
        id: "tool-1",
        role: "tool",
        callId: "call-1",
        name: "mcp__private_server__read_a_sensitive_path",
        status: "completed",
      },
      { id: "tool-2", role: "tool", callId: "call-2", name: "web_search", status: "completed" },
    ];
    const markup = renderToStaticMarkup(
      <MessageList
        messages={messages}
        folderAccessRequests={[]}
        nativeHost={false}
        nativeBusy={false}
        resolvingFolderCalls={new Set()}
        folderAccessErrors={{}}
        busy={false}
        scrollRef={{ current: null }}
        onScroll={noop}
        onApproval={noop}
        onFolderAccessDecision={noop}
        onFolderAccessCancel={noop}
      />,
    );

    expect(markup).toContain("Used a tool and searched the web");
    expect(markup).toContain("Use a tool");
    expect(markup).toContain("Tool complete");
    expect(markup).not.toContain("private_server");
    expect(markup).not.toContain("sensitive_path");
  });

  it("renders each source beneath only its owning assistant message", () => {
    const messages: ChatMessage[] = [
      {
        id: "assistant-1",
        role: "assistant",
        text: "First answer",
        sources: [
          {
            id: "private-citation-id",
            ordinal: 1,
            excerpt: "First source excerpt",
            heading: "First source",
            pages: [4],
          },
        ],
      },
      {
        id: "assistant-2",
        role: "assistant",
        text: "Second answer",
        sources: [],
      },
    ];
    const markup = renderToStaticMarkup(
      <MessageList
        messages={messages}
        folderAccessRequests={[]}
        nativeHost={false}
        nativeBusy={false}
        resolvingFolderCalls={new Set()}
        folderAccessErrors={{}}
        busy={false}
        scrollRef={{ current: null }}
        onScroll={noop}
        onApproval={noop}
        onFolderAccessDecision={noop}
        onFolderAccessCancel={noop}
      />,
    );

    expect(markup.indexOf("First answer")).toBeLessThan(
      markup.indexOf("First source excerpt"),
    );
    expect(markup.indexOf("First source excerpt")).toBeLessThan(
      markup.indexOf("Second answer"),
    );
    expect(markup).not.toContain("private-citation-id");
    expect(markup).not.toContain("ow-source");
  });

  it("shows settled message actions without treating the active response as finished", () => {
    const messages: ChatMessage[] = [
      {
        id: "user-1",
        role: "user",
        text: "Earlier question",
        createdAt: "2026-07-20T10:00:00Z",
      },
      {
        id: "assistant-settled",
        role: "assistant",
        text: "Earlier answer",
        sources: [],
        createdAt: "2026-07-20T10:00:01Z",
      },
      {
        id: "assistant-streaming",
        role: "assistant",
        text: "Partial answer",
        sources: [],
        createdAt: "2026-07-20T10:01:00Z",
      },
    ];
    const markup = renderToStaticMarkup(
      <MessageList
        messages={messages}
        folderAccessRequests={[]}
        nativeHost={false}
        nativeBusy={false}
        resolvingFolderCalls={new Set()}
        folderAccessErrors={{}}
        busy
        scrollRef={{ current: null }}
        onScroll={noop}
        onApproval={noop}
        onFolderAccessDecision={noop}
        onFolderAccessCancel={noop}
      />,
    );

    expect(markup.match(/aria-label="Copy"/g)).toHaveLength(1);
    expect(markup.match(/class="message-footer"/g)).toHaveLength(2);
    expect(markup).toContain('class="message-user-frame"');
    expect(markup).not.toContain('dateTime="2026-07-20T10:01:00Z"');
  });

  it("does not add message actions to tool, system, or error rows", () => {
    const messages: ChatMessage[] = [
      {
        id: "tool-1",
        role: "tool",
        callId: "call-1",
        name: "read_file",
        status: "running",
      },
      { id: "system-1", role: "system", text: "turn cancelled" },
      { id: "error-1", role: "error", text: "could not complete" },
    ];
    const markup = renderToStaticMarkup(
      <MessageList
        messages={messages}
        folderAccessRequests={[]}
        nativeHost={false}
        nativeBusy={false}
        resolvingFolderCalls={new Set()}
        folderAccessErrors={{}}
        busy={false}
        scrollRef={{ current: null }}
        onScroll={noop}
        onApproval={noop}
        onFolderAccessDecision={noop}
        onFolderAccessCancel={noop}
      />,
    );

    expect(markup).not.toContain("message-footer");
    expect(markup).not.toContain("Copy");
  });

  it("keeps the previous assistant settled while a submitted user message awaits turn start", () => {
    const messages: ChatMessage[] = [
      {
        id: "assistant-settled",
        role: "assistant",
        text: "Previous answer",
        sources: [],
        createdAt: "2026-07-20T10:00:00Z",
      },
      {
        id: "user-optimistic",
        role: "user",
        text: "Next question",
        createdAt: "2026-07-20T10:01:00Z",
      },
    ];
    const markup = renderToStaticMarkup(
      <MessageList
        messages={messages}
        folderAccessRequests={[]}
        nativeHost={false}
        nativeBusy={false}
        resolvingFolderCalls={new Set()}
        folderAccessErrors={{}}
        busy
        scrollRef={{ current: null }}
        onScroll={noop}
        onApproval={noop}
        onFolderAccessDecision={noop}
        onFolderAccessCancel={noop}
      />,
    );

    expect(markup).toContain('aria-label="Copy"');
    expect(markup).toContain('dateTime="2026-07-20T10:00:00Z"');
    expect(markup).toContain('dateTime="2026-07-20T10:01:00Z"');
  });
});
