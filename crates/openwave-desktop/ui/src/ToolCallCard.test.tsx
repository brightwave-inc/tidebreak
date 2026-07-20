import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import {
  ToolCallCard,
  toolApprovalPresentation,
} from "./ToolCallCard";

describe("ToolCallCard", () => {
  it("uses an allowlisted presentation and a polite live status", () => {
    const markup = renderToStaticMarkup(
      <ToolCallCard name="web_search" status="running" />,
    );

    expect(markup).toContain("Search the web");
    expect(markup).toContain("Searching the web");
    expect(markup).toContain('role="status"');
    expect(markup).toContain('aria-live="polite"');
    expect(markup).toContain('aria-atomic="true"');
  });

  it("does not expose an unknown tool name", () => {
    const markup = renderToStaticMarkup(
      <ToolCallCard
        name="mcp__private_server__read_a_sensitive_path"
        status="completed"
      />,
    );

    expect(markup).toContain("Use a tool");
    expect(markup).toContain("Tool complete");
    expect(markup).not.toContain("private_server");
    expect(markup).not.toContain("sensitive_path");
  });

  it("renders the fixed local-search presentation", () => {
    const markup = renderToStaticMarkup(
      <ToolCallCard name="search" status="completed" />,
    );

    expect(markup).toContain("Search documents");
    expect(markup).toContain("Document search complete");
  });

  it("degrades an unknown runtime status without using it as copy or a class", () => {
    const markup = renderToStaticMarkup(
      <ToolCallCard
        name="web_search"
        status={"private-provider-diagnostic" as "completed"}
      />,
    );

    expect(markup).toContain("Status unavailable");
    expect(markup).toContain("is-unknown");
    expect(markup).not.toContain("private-provider-diagnostic");
  });

  it("allows approval only for a fixed action description", () => {
    expect(toolApprovalPresentation("search_may_share_query_and_excerpts")).toEqual({
      summary:
        "Allow search to send your query and potentially matching document excerpts to configured AI services outside OpenWave?",
      canApprove: true,
    });
    expect(toolApprovalPresentation("unsupported")).toEqual({
      summary: "The exact action cannot be safely described.",
      canApprove: false,
    });
  });
});
