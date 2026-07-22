import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import {
  ToolCallCard,
  toolApprovalPresentation,
} from "./ToolCallCard";

// Strip element markup so leak checks assert on the visible copy only. The
// status glyph is an inline SVG whose element names (e.g. `<path>`) are not
// user-facing text and must not be mistaken for a leaked resource path.
function visibleText(markup: string): string {
  return markup.replace(/<[^>]*>/g, "");
}

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

  it("renders a fixed delegated-file presentation without resource details", () => {
    const markup = renderToStaticMarkup(
      <ToolCallCard name="read_delegated_file" status="running" />,
    );

    expect(markup).toContain("Read a delegated file");
    expect(markup).toContain("Reading a delegated file");
    expect(visibleText(markup)).not.toContain("path");
    expect(visibleText(markup)).not.toContain("root");
  });

  it("distinguishes delegation from waiting for the child results", () => {
    const delegated = renderToStaticMarkup(
      <ToolCallCard name="spawn_sandbox_agent" status="completed" />,
    );
    const waiting = renderToStaticMarkup(
      <ToolCallCard name="wait_for_agents" status="running" />,
    );
    const finished = renderToStaticMarkup(
      <ToolCallCard name="wait_for_agents" status="completed" />,
    );

    expect(delegated).toContain("Task delegated");
    expect(delegated).not.toContain("task complete");
    expect(waiting).toContain("Waiting for background agents");
    expect(finished).toContain("Background agents finished");
  });

  it("uses fixed terminal failure copy for background-agent waits", () => {
    const failed = renderToStaticMarkup(
      <ToolCallCard name="wait_for_agents" status="failed" />,
    );
    const cancelled = renderToStaticMarkup(
      <ToolCallCard name="wait_for_agents" status="cancelled" />,
    );

    expect(failed).toContain("Tool could not complete");
    expect(cancelled).toContain("Not run");
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
    expect(
      toolApprovalPresentation("exec_may_run_networked_command"),
    ).toEqual({
      summary:
        "Allow OpenWave to run a command that leaves the chat workspace and may reach the network?",
      canApprove: true,
    });
    expect(toolApprovalPresentation("unsupported")).toEqual({
      summary: "The exact action cannot be safely described.",
      canApprove: false,
    });
  });
});
