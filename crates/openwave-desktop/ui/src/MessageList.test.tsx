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
        message={{ id: "assistant-1", role: "assistant", text: "## Answer" }}
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
      { id: "assistant-2", role: "assistant", text: "" },
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
      { id: "assistant-1", role: "assistant", text: "Done" },
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

    expect(markup).toContain('class="tool-activity-group"');
    expect(markup).toContain('aria-expanded="false"');
    expect(markup).toContain('aria-controls="tool-activity-group-0"');
    expect(markup).toContain('id="tool-activity-group-0" hidden=""');
    expect(markup).toContain("2 tool activities");
    expect(markup).toContain("Search the web: Web search complete");
    expect(markup).toContain("Read a file: Tool could not complete");
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

    expect(markup).toContain("Use a tool: Tool complete");
    expect(markup).not.toContain("private_server");
    expect(markup).not.toContain("sensitive_path");
  });
});
