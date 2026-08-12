import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import {
  copyMessageContent,
  formatMessageTimestamp,
  MessageFooter,
} from "./MessageFooter";

describe("MessageFooter", () => {
  it("renders copy and a machine-readable timestamp for a settled assistant message", () => {
    const markup = renderToStaticMarkup(
      <MessageFooter
        role="assistant"
        text="## Finished answer"
        createdAt="2026-07-20T10:00:00Z"
      />,
    );

    expect(markup).toContain('aria-label="Copy"');
    expect(markup).toContain('dateTime="2026-07-20T10:00:00Z"');
    expect(markup).toContain('aria-live="polite"');
  });

  it("does not expose actions or time for an unsettled assistant response", () => {
    const markup = renderToStaticMarkup(
      <MessageFooter
        role="assistant"
        text="Partial answer"
        createdAt="2026-07-20T10:00:00Z"
        settled={false}
      />,
    );

    expect(markup).toBe("");
  });

  it("leaves a mid-turn assistant fragment bare", () => {
    const markup = renderToStaticMarkup(
      <MessageFooter
        role="assistant"
        text="Interim answer"
        createdAt="2026-07-20T10:00:00Z"
        sequenceEnd={false}
      />,
    );

    expect(markup).toBe("");
  });

  it("does not render a timestamp-only footer for an empty settled assistant", () => {
    const markup = renderToStaticMarkup(
      <MessageFooter
        role="assistant"
        text="  "
        createdAt="2026-07-20T10:00:00Z"
      />,
    );

    expect(markup).toBe("");
  });

  it("rejects malformed durable timestamps instead of rendering bad text", () => {
    expect(
      formatMessageTimestamp("not-a-date", new Date("2026-07-20T12:00:00Z")),
    ).toBeNull();
  });

  it("copies rendered HTML beside the message's plain text", async () => {
    const writes: ClipboardItem[][] = [];
    class TestClipboardItem {
      readonly types: string[];
      constructor(readonly data: Record<string, Blob>) {
        this.types = Object.keys(data);
      }
      async getType(type: string): Promise<Blob> {
        return this.data[type] ?? new Blob();
      }
    }
    const clipboard = {
      writeText: async () => undefined,
      write: async (items: ClipboardItem[]) => {
        writes.push(items);
      },
    };

    await copyMessageContent(
      "<table><tbody><tr><td>Total</td></tr></tbody></table>",
      "| Total |",
      clipboard,
      TestClipboardItem as unknown as typeof ClipboardItem,
    );

    expect(writes).toHaveLength(1);
    expect(writes[0]?.[0]?.types).toEqual(["text/plain", "text/html"]);
  });
});
