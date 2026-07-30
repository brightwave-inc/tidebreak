// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { TranscriptImageAttachments } from "./TranscriptImageAttachments";

// Registered before cleanup so the LIFO afterEach order unmounts (revoking
// the stubbed object URL) before the URL global is restored.
afterEach(() => vi.unstubAllGlobals());
afterEach(cleanup);

// The renderer pins a cross-boundary contract: it only draws the image when
// the served Content-Type matches the media type in the transcript record.
const image = {
  attachmentId: "attachment-1",
  mediaType: "image/png",
  width: 320,
  height: 240,
};

describe("TranscriptImageAttachments served content type", () => {
  it("renders the image when the served blob type matches the transcript record", async () => {
    vi.stubGlobal("URL", {
      createObjectURL: vi.fn(() => "blob:transcript-image"),
      revokeObjectURL: vi.fn(),
    });
    const client = {
      getChatImageAttachment: vi.fn(
        async () => new Blob(["pixels"], { type: "image/png" }),
      ),
    };
    render(
      <TranscriptImageAttachments
        client={client}
        chatId="chat-1"
        images={[image]}
      />,
    );

    const rendered = await screen.findByRole("img", {
      name: "Attached image 1: 320 by 240 pixels",
    });
    expect(rendered).toHaveAttribute("src", "blob:transcript-image");
  });

  it("shows the unavailable state when the served blob type disagrees with the transcript record", async () => {
    const createObjectURL = vi.fn(() => "blob:transcript-image");
    vi.stubGlobal("URL", { createObjectURL, revokeObjectURL: vi.fn() });
    const client = {
      getChatImageAttachment: vi.fn(
        async () => new Blob(["pixels"], { type: "application/octet-stream" }),
      ),
    };
    render(
      <TranscriptImageAttachments
        client={client}
        chatId="chat-1"
        images={[image]}
      />,
    );

    expect(
      await screen.findByText("Attached image unavailable"),
    ).toBeInTheDocument();
    expect(screen.queryByRole("img")).toBeNull();
    expect(createObjectURL).not.toHaveBeenCalled();
  });
});
