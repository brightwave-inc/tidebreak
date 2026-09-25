// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { FileBytesSource } from "@/document/useFileDownload";
import { MarkdownViewer } from "./markdown-viewer";

afterEach(cleanup);

describe("MarkdownViewer download failures", () => {
  it("downloads the file again when the reader tries again", async () => {
    const fetch = vi
      .fn<FileBytesSource["fetch"]>()
      .mockRejectedValueOnce(new TypeError("Load failed"))
      .mockResolvedValue({
        bytes: new TextEncoder().encode("Renewal plan"),
        contentType: "text/plain",
      });
    render(
      <MarkdownViewer
        source={{
          id: "document-retry",
          // A key no other test fills, so the shared byte cache starts empty.
          cacheKey: `document/retry-${crypto.randomUUID()}`,
          fetch,
        }}
      />,
    );

    const failure = await screen.findByRole("alert");
    expect(failure).toHaveTextContent("This document could not be loaded.");
    await userEvent.click(screen.getByRole("button", { name: "Try again" }));

    expect(await screen.findByText("Renewal plan")).toBeInTheDocument();
    expect(fetch).toHaveBeenCalledTimes(2);
    expect(screen.queryByRole("alert")).toBeNull();
  });
});
