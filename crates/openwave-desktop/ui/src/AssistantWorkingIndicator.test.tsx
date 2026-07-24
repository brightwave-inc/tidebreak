import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { AssistantWorkingIndicator } from "./AssistantWorkingIndicator";
import {
  shouldShowAssistantWorking,
  type ChatMessage,
} from "./MessageList";

describe("AssistantWorkingIndicator", () => {
  it("renders fixed visible copy as one polite atomic status", () => {
    const markup = renderToStaticMarkup(<AssistantWorkingIndicator />);

    expect(markup).toContain("Working");
    expect(markup).toContain('role="status"');
    expect(markup).toContain('aria-live="polite"');
    expect(markup).toContain('aria-atomic="true"');
    expect(markup).toContain('aria-hidden="true"');
  });

  it("shows while an open turn has no more specific live status", () => {
    const messages: ChatMessage[] = [
      { id: "user", role: "user", text: "Question" },
      { id: "assistant", role: "assistant", text: "", sources: [] },
    ];

    expect(shouldShowAssistantWorking(messages, true, 0)).toBe(true);
    expect(
      shouldShowAssistantWorking(
        [{ id: "user-only", role: "user", text: "Submitted question" }],
        true,
        0,
      ),
    ).toBe(true);
    expect(shouldShowAssistantWorking(messages, false, 0)).toBe(false);
  });

  it("does not compete with a partial assistant response", () => {
    expect(
      shouldShowAssistantWorking(
        [
          {
            id: "assistant-streaming",
            role: "assistant",
            text: "Partial response",
            sources: [],
          },
        ],
        true,
        0,
      ),
    ).toBe(false);
    expect(
      shouldShowAssistantWorking(
        [
          {
            id: "assistant-sourced",
            role: "assistant",
            text: "",
            sources: [
              {
                id: "source",
                ordinal: 1,
                excerpt: "Visible evidence",
                heading: null,
                pages: [],
              },
            ],
          },
        ],
        true,
        0,
      ),
    ).toBe(false);
  });

  it("defers to active tool and user-action statuses", () => {
    const runningTool: ChatMessage = {
      id: "tool",
      role: "tool",
      callId: "call",
      name: "web_search",
      status: "running",
    };
    const approval: ChatMessage = {
      id: "approval",
      role: "approval",
      callId: "call",
      summary: "Safe approval copy",
      canApprove: true,
      canRemember: true,
    };

    expect(shouldShowAssistantWorking([runningTool], true, 0)).toBe(false);
    expect(shouldShowAssistantWorking([approval], true, 0)).toBe(false);
    expect(shouldShowAssistantWorking([], true, 1)).toBe(false);
    expect(
      shouldShowAssistantWorking(
        [
          {
            ...runningTool,
            status: "future-private-status" as "running",
          },
        ],
        true,
        0,
      ),
    ).toBe(false);
  });

  it("returns after a terminal tool phase before the next assistant segment", () => {
    const messages: ChatMessage[] = [
      {
        id: "assistant-before-tool",
        role: "assistant",
        text: "I will check that.",
        sources: [],
      },
      {
        id: "tool",
        role: "tool",
        callId: "call",
        name: "private-provider-tool-name",
        status: "completed",
      },
      {
        id: "approval",
        role: "approval",
        callId: "call",
        summary: "private-provider-diagnostic",
        canApprove: false,
        canRemember: false,
        resolved: true,
      },
    ];

    expect(shouldShowAssistantWorking(messages, true, 0)).toBe(true);
  });
});

describe("thinking variant", () => {
  it("announces thinking while reasoning is active", () => {
    expect(
      renderToStaticMarkup(<AssistantWorkingIndicator thinking />),
    ).toContain("Thinking…");
    expect(renderToStaticMarkup(<AssistantWorkingIndicator />)).toContain(
      "Working",
    );
  });
});
