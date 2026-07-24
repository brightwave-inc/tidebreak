import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import {
  ToolCommandCard,
  toolCallPresentation,
  toolApprovalPresentation,
} from "./ToolCallCard";

// Strip element markup so leak checks assert on the visible copy only. The
// status glyph is an inline SVG whose element names (e.g. `<path>`) are not
// user-facing text and must not be mistaken for a leaked resource path.
function visibleText(markup: string): string {
  return markup.replace(/<[^>]*>/g, "");
}

const preview = {
  tool: "exec" as const,
  command: "cargo",
  args: ["test", "--workspace"],
  cwd: "checkout",
};

describe("toolCallPresentation", () => {
  it("names a tool in the tense its status calls for", () => {
    expect(toolCallPresentation("web_search", "running").title).toBe(
      "Searching the web",
    );
    expect(toolCallPresentation("web_search", "completed").title).toBe(
      "Searched the web",
    );
    // A failure keeps the untensed phrase: the tool did not do the thing, so
    // naming it in the past would overstate what happened.
    expect(toolCallPresentation("web_search", "failed").title).toBe(
      "Search the web",
    );
    expect(toolCallPresentation("web_search", "cancelled").title).toBe(
      "Search the web",
    );
  });

  it("does not expose an unknown tool name", () => {
    const presentation = toolCallPresentation(
      "mcp__private_server__read_a_sensitive_path",
      "completed",
    );

    expect(presentation.title).toBe("Used a tool");
    expect(JSON.stringify(presentation)).not.toContain("private_server");
    expect(JSON.stringify(presentation)).not.toContain("sensitive_path");
  });

  it("distinguishes source discovery from direct source reads", () => {
    expect(toolCallPresentation("list_sources", "running").title).toBe(
      "Checking sources",
    );
    expect(toolCallPresentation("read_source", "completed").title).toBe(
      "Read a source",
    );
  });

  it("distinguishes delegation from waiting for the child results", () => {
    expect(toolCallPresentation("spawn_sandbox_agent", "completed").title).toBe(
      "Task delegated",
    );
    expect(toolCallPresentation("wait_for_agents", "running").title).toBe(
      "Waiting for background agents",
    );
  });

  it("degrades an unknown runtime status without using it as copy", () => {
    const presentation = toolCallPresentation(
      "web_search",
      "private-provider-diagnostic" as "completed",
    );

    expect(presentation.tone).toBe("unknown");
    expect(presentation.badgeLabel).toBe("Status unavailable");
    expect(JSON.stringify(presentation)).not.toContain(
      "private-provider-diagnostic",
    );
  });
});

describe("ToolCommandCard", () => {
  it("titles itself with the command instead of a generic phrase", () => {
    const markup = renderToStaticMarkup(
      <ToolCommandCard name="exec" status="completed" preview={preview} />,
    );

    expect(visibleText(markup)).toContain("cargo test --workspace");
    expect(visibleText(markup)).not.toContain("Ran a command");
    // The allowlisted phrase still names the card for assistive technology.
    expect(markup).toContain('aria-label="Run a command: Command complete"');
  });

  it("opens while the command is running and closes once it has settled", () => {
    const running = renderToStaticMarkup(
      <ToolCommandCard name="exec" status="running" preview={preview} />,
    );
    const done = renderToStaticMarkup(
      <ToolCommandCard name="exec" status="completed" preview={preview} />,
    );

    expect(running).toContain('aria-expanded="true"');
    expect(visibleText(running)).toContain("# working directory: checkout");
    expect(done).toContain('aria-expanded="false"');
  });

  it("carries the outcome in a badge rather than a second line of prose", () => {
    for (const [status, badge] of [
      ["running", "Running…"],
      ["waiting_approval", "Waiting for approval"],
      ["completed", "Done"],
      ["failed", "Failed"],
      ["cancelled", "Not run"],
    ] as const) {
      const markup = renderToStaticMarkup(
        <ToolCommandCard name="exec" status={status} preview={preview} />,
      );
      expect(visibleText(markup)).toContain(badge);
    }
  });
});

describe("toolApprovalPresentation", () => {
  it("allows approval only for a fixed action description", () => {
    expect(
      toolApprovalPresentation("search_may_share_query_and_excerpts"),
    ).toEqual({
      summary:
        "Allow search to send your query and potentially matching document excerpts to configured AI services outside OpenWave?",
      canApprove: true,
      canRemember: true,
    });
    expect(toolApprovalPresentation("web_search_may_share_query")).toEqual({
      summary:
        "Allow web search to send this query and its explicit filters to the configured search provider outside OpenWave?",
      canApprove: true,
      canRemember: true,
    });
    expect(toolApprovalPresentation("exec_may_run_networked_command")).toEqual({
      summary:
        "Allow OpenWave to run a command that leaves the chat workspace and may reach the network?",
      canApprove: true,
      canRemember: true,
    });
    expect(toolApprovalPresentation("external_mcp_may_call_server")).toEqual({
      summary:
        "Allow this external MCP server to receive the call and act with its own local or remote permissions?",
      canApprove: true,
      canRemember: false,
    });
    expect(toolApprovalPresentation("unsupported")).toEqual({
      summary: "The exact action cannot be safely described.",
      canApprove: false,
      canRemember: false,
    });
  });
});
