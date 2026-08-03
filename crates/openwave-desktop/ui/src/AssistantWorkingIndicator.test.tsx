import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { AssistantWorkingIndicator } from "./AssistantWorkingIndicator";
import {
  shouldShowAssistantWorking,
  type ChatMessage,
} from "./MessageList";

describe("AssistantWorkingIndicator", () => {
  it("carries the default status for assistive tech without a visible label", () => {
    const markup = renderToStaticMarkup(<AssistantWorkingIndicator />);

    // The label is announced but visually hidden — the logomark stands alone.
    expect(markup).toContain("Working");
    expect(markup).toContain('class="sr-only"');
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
                documentId: "document-1",
                locator: { kind: "document" },
              },
            ],
          },
        ],
        true,
        0,
      ),
    ).toBe(false);
  });

  it("returns under a partial response when the stream has stalled", () => {
    const partial: ChatMessage[] = [
      {
        id: "assistant-streaming",
        role: "assistant",
        text: "Partial response",
        sources: [],
      },
    ];

    expect(shouldShowAssistantWorking(partial, true, 0, true)).toBe(true);
    // A stall never overrides a more specific live status or a closed turn.
    expect(
      shouldShowAssistantWorking(
        [
          ...partial,
          {
            id: "tool",
            role: "tool",
            callId: "call",
            name: "web_search",
            status: "running",
          },
        ],
        true,
        0,
        true,
      ),
    ).toBe(false);
    expect(shouldShowAssistantWorking(partial, false, 0, true)).toBe(false);
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
