import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { ToolCallCard } from "./ToolCallCard";

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
});
