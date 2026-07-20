import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import {
  ClipboardCopyButton,
  COPY_STATE_RESET_MS,
  copyPlainText,
  scheduleCopyStateReset,
} from "./ClipboardCopyButton";

describe("ClipboardCopyButton", () => {
  it("renders a named action with a polite result status", () => {
    const markup = renderToStaticMarkup(
      <ClipboardCopyButton
        value="Visible text"
        label="Copy table contents"
        copiedAnnouncement="Table copied to clipboard."
        failedAnnouncement="Table could not be copied."
      />,
    );

    expect(markup).toContain('aria-label="Copy table contents"');
    expect(markup).toContain('title="Copy table contents"');
    expect(markup).toContain('role="status"');
    expect(markup).toContain('aria-live="polite"');
  });

  it("copies exact plain text and reports unavailable clipboard access", async () => {
    const writeText = vi.fn(async () => undefined);

    await copyPlainText("Name\tValue\nAlpha\t1\n", { writeText });

    expect(writeText).toHaveBeenCalledWith("Name\tValue\nAlpha\t1\n");
    await expect(copyPlainText("text", undefined)).rejects.toThrow(
      "Clipboard access is unavailable",
    );
  });

  it("resets after the bounded success or failure interval and cleans up", () => {
    let callback: (() => void) | undefined;
    const reset = vi.fn();
    const clearTimeout = vi.fn();
    const setTimeout = vi.fn((next: () => void, delayMs: number) => {
      callback = next;
      expect(delayMs).toBe(COPY_STATE_RESET_MS);
      return 42;
    });

    const cleanup = scheduleCopyStateReset(reset, {
      setTimeout,
      clearTimeout,
    });
    callback?.();
    cleanup();

    expect(reset).toHaveBeenCalledTimes(1);
    expect(clearTimeout).toHaveBeenCalledWith(42);
  });
});
