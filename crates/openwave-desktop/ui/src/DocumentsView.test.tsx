import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { DocumentsView } from "./DocumentsView";

describe("DocumentsView", () => {
  it("presents sources as context owned by the current conversation", () => {
    const markup = renderToStaticMarkup(
      <DocumentsView chatId="chat-1" onBack={vi.fn()} />,
    );

    expect(markup).toContain("This conversation");
    expect(markup).toContain(">Sources<");
    expect(markup).toContain(
      "Add files for OpenWave to use in this conversation.",
    );
    expect(markup).toContain("Conversation sources");
    expect(markup).toContain("Loading sources for this conversation");
    expect(markup).not.toContain("Local library");
    expect(markup).not.toContain("Your documents");
  });
});
