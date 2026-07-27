import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { AgentRun } from "./api";
import { MessageBubble, MessageList, type ChatMessage } from "./MessageList";

const noop = () => undefined;

function backgroundRun(
  id: string,
  spawnCallId: string,
  status: AgentRun["status"],
): AgentRun {
  return {
    id,
    parent_id: "foreground",
    spawn_call_id: spawnCallId,
    execution: "sandbox",
    status,
    started_at: null,
    finished_at: null,
    last_error_code: null,
    activity: null,
    created_at: "2026-07-27T12:00:00Z",
    updated_at: "2026-07-27T12:00:00Z",
  };
}

describe("MessageBubble", () => {
  it("keeps the user compact while rendering assistant Markdown without a bubble", () => {
    const user = renderToStaticMarkup(
      <MessageBubble
        message={{ id: "user-1", role: "user", text: "Hello **there**" }}
        busy={false}
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
      />,
    );

    expect(user).toContain('class="message message-user"');
    expect(user).toContain("<strong>there</strong>");
    expect(assistant).toContain('class="message message-assistant"');
    expect(assistant).toContain("<h2>Answer</h2>");
    expect(assistant).not.toContain("bubble");
  });

  it("keeps a phase ahead of the response it precedes, with one worker status", () => {
    const messages: ChatMessage[] = [
      { id: "tool-1", role: "tool", callId: "call-1", name: "web_search", status: "running" },
      {
        id: "approval-1",
        role: "approval",
        callId: "call-1",
        summary: "Search a site",
        canApprove: true,
        canRemember: true,
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
        decidingApprovalCalls={new Set()}
        approvalErrors={{}}
        busy
        scrollRef={{ current: null }}
        onScroll={noop}
        onApproval={noop}
        onFolderAccessDecision={noop}
        onFolderAccessCancel={noop}
      />,
    );

    // The parked call is represented by its approval card alone: no rail line
    // repeating the pending action, and no generic worker status either.
    expect(markup).toContain('aria-label="Approval needed"');
    expect(markup).toContain("Yes, allow it once");
    expect(markup).not.toContain("Searching the web");
    expect(markup).not.toContain("Working");
  });

  it("replaces an empty active assistant placeholder with one worker status", () => {
    const markup = renderToStaticMarkup(
      <MessageList
        messages={[
          { id: "assistant-active", role: "assistant", text: "", sources: [] },
        ]}
        folderAccessRequests={[]}
        nativeHost={false}
        nativeBusy={false}
        resolvingFolderCalls={new Set()}
        folderAccessErrors={{}}
        decidingApprovalCalls={new Set()}
        approvalErrors={{}}
        busy
        scrollRef={{ current: null }}
        onScroll={noop}
        onApproval={noop}
        onFolderAccessDecision={noop}
        onFolderAccessCancel={noop}
      />,
    );

    expect(markup.match(/>Working</g)).toHaveLength(1);
    expect(markup).not.toContain('aria-label="Assistant"');
    expect(markup).not.toContain("message-stream-placeholder");
  });

  it("shows a specific background wait instead of the generic worker status", () => {
    const markup = renderToStaticMarkup(
      <MessageList
        messages={[
          {
            id: "wait-1",
            role: "tool",
            callId: "call-1",
            name: "wait_for_agents",
            status: "running",
          },
        ]}
        folderAccessRequests={[]}
        nativeHost={false}
        nativeBusy={false}
        resolvingFolderCalls={new Set()}
        folderAccessErrors={{}}
        decidingApprovalCalls={new Set()}
        approvalErrors={{}}
        busy
        scrollRef={{ current: null }}
        onScroll={noop}
        onApproval={noop}
        onFolderAccessDecision={noop}
        onFolderAccessCancel={noop}
      />,
    );

    expect(markup).toContain("Waiting for background agents");
    expect(markup).not.toContain(">Working<");
    expect(markup).not.toContain("call-1");
  });

  it("splits phases at the response between them", () => {
    const messages: ChatMessage[] = [
      { id: "tool-1", role: "tool", callId: "call-1", name: "web_search", status: "completed" },
      { id: "tool-2", role: "tool", callId: "call-2", name: "read_file", status: "failed" },
      { id: "assistant-1", role: "assistant", text: "Done", sources: [] },
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

    expect(markup).toContain('aria-controls="tool-activity-group-0"');
    expect(markup).toContain('aria-controls="tool-activity-group-1"');
    expect(markup).toContain("Read a file and 1 other task");
    expect(markup.indexOf("Read a file and 1 other task")).toBeLessThan(
      markup.indexOf("Done"),
    );
    expect(markup.indexOf("Done")).toBeLessThan(markup.indexOf("Browse files"));
  });

  it("keeps a live call in the same phase as the settled ones around it", () => {
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

    // One phase, in the present because part of it is still live.
    expect(markup).toContain('aria-controls="tool-activity-group-0"');
    expect(markup).not.toContain('aria-controls="tool-activity-group-1"');
    expect(markup).toContain("Reading a file and 3 other tasks");
    // The phase label is the running commentary, so it is announced politely
    // as the agent moves through the phase.
    expect(markup).toContain('aria-live="polite"');
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

    expect(markup).toContain("Searched the web and 1 other task");
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
        decidingApprovalCalls={new Set()}
        approvalErrors={{}}
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
        decidingApprovalCalls={new Set()}
        approvalErrors={{}}
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

describe("background-agent transcript activity", () => {
  it("hangs the durable status card below its exact delegation phase", () => {
    const markup = renderToStaticMarkup(
      <MessageList
        messages={[
          {
            id: "spawn-row",
            role: "tool",
            callId: "spawn-call",
            name: "spawn_sandbox_agent",
            status: "completed",
          },
          { id: "assistant", role: "assistant", text: "I will wait.", sources: [] },
        ]}
        folderAccessRequests={[]}
        nativeHost={false}
        nativeBusy={false}
        resolvingFolderCalls={new Set()}
        folderAccessErrors={{}}
        decidingApprovalCalls={new Set()}
        approvalErrors={{}}
        backgroundAgentRuns={[backgroundRun("run-1", "spawn-call", "running")]}
        busy={false}
        scrollRef={{ current: null }}
        onScroll={noop}
        onApproval={noop}
        onFolderAccessDecision={noop}
        onFolderAccessCancel={noop}
      />,
    );

    expect(markup).toContain("1 background agent");
    expect(markup).toContain("Working in the background");
    expect(markup.indexOf("1 background agent")).toBeLessThan(
      markup.indexOf("I will wait."),
    );
  });
});

describe("superseded responses", () => {
  it("renders a superseded bubble dimmed with an accessible label", () => {
    const markup = renderToStaticMarkup(
      <MessageBubble
        message={{
          id: "a1",
          role: "assistant",
          text: "abandoned partial",
          sources: [],
          superseded: true,
        }}
        busy={false}
      />,
    );
    expect(markup).toContain("message-superseded");
    expect(markup).toContain("Superseded response");
    expect(markup).toContain("abandoned partial");
  });
});
