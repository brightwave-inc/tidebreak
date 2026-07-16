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
      { id: "approval-1", role: "approval", callId: "call-1", summary: "Search a site" },
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
});
