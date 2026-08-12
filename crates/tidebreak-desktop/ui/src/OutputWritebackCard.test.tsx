import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

import { OutputWritebackCard } from "./OutputWritebackCard";
import type { PendingOutputWritebackRequest } from "./api";

function request(
  mode: PendingOutputWritebackRequest["mode"],
): PendingOutputWritebackRequest {
  return { callId: "call-1", turnId: "turn-1", mode, claimedByDesktop: false };
}

describe("OutputWritebackCard", () => {
  it("names the replacement without rendering destination data", () => {
    const html = renderToStaticMarkup(
      <OutputWritebackCard
        request={request("replace")}
        nativeHost
        working={false}
        error={undefined}
        onDecision={vi.fn()}
        onCancel={vi.fn()}
      />,
    );

    expect(html).toContain("Replace an existing file?");
    expect(html).toContain("Allow replacement");
    expect(html).not.toContain("output_id");
    expect(html).not.toContain("root_id");
    expect(html).not.toContain("Documents/");
  });

  it("asks about a create as a write rather than a replacement", () => {
    const html = renderToStaticMarkup(
      <OutputWritebackCard
        request={request("create")}
        nativeHost
        working={false}
        error={undefined}
        onDecision={vi.fn()}
        onCancel={vi.fn()}
      />,
    );

    expect(html).toContain("Write a file to a connected folder?");
    expect(html).toContain("Allow write");
    expect(html).not.toContain("Replace an existing file?");
  });

  it("fails closed in browser-only mode", () => {
    const html = renderToStaticMarkup(
      <OutputWritebackCard
        request={request("replace")}
        nativeHost={false}
        working={false}
        error={undefined}
        onDecision={vi.fn()}
        onCancel={vi.fn()}
      />,
    );

    expect(html).toContain("unavailable in browser-only mode");
    expect(html).toContain("Cancel turn");
    expect(html).not.toContain("Allow replacement");
  });
});
