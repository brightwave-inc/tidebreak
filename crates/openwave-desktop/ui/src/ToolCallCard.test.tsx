import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { ExecResultPreview } from "./api";
import { toolPreviewPresentation } from "./ToolPreview";
import {
  ToolCommandCard,
  commandOutput,
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
      <ToolCommandCard name="exec" status="completed" preview={preview} result={null} />,
    );

    expect(visibleText(markup)).toContain("cargo test --workspace");
    expect(visibleText(markup)).not.toContain("Ran a command");
    // The allowlisted phrase still names the card for assistive technology.
    expect(markup).toContain('aria-label="Run a command: Command complete"');
  });

  it("opens while the command is running and closes once it has settled", () => {
    const running = renderToStaticMarkup(
      <ToolCommandCard name="exec" status="running" preview={preview} result={null} />,
    );
    const done = renderToStaticMarkup(
      <ToolCommandCard name="exec" status="completed" preview={preview} result={null} />,
    );

    expect(running).toContain('aria-expanded="true"');
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
        <ToolCommandCard name="exec" status={status} preview={preview} result={null} />,
      );
      expect(visibleText(markup)).toContain(badge);
    }
  });

  it("renders exec preview images inside the output surface", () => {
    const markup = renderToStaticMarkup(
      <ToolCommandCard
        name="exec"
        status="completed"
        preview={preview}
        result={{
          tool: "exec",
          exitCode: 0,
          timedOut: false,
          outputTruncated: false,
          stdout: "",
          stderr: "",
          images: [
            {
              attachmentId: "preview-1",
              mediaType: "image/png",
              width: 800,
              height: 600,
            },
          ],
        }}
        imageClient={{
          getChatImageAttachment: async () => new Blob([], { type: "image/png" }),
        }}
        chatId="chat-1"
      />,
    );

    expect(markup).toContain('aria-label="Command preview images"');
    expect(markup).toContain(
      'aria-label="Command preview 1: 800 by 600 pixels"',
    );
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

describe("command output", () => {
  const ran = {
    tool: "exec" as const,
    exitCode: 0,
    timedOut: false,
    outputTruncated: false,
    stdout: "",
    stderr: "",
  };

  it("labels each captured stream and keeps them in order", () => {
    // Each stream's own trailing newline is trimmed, so the two sections are
    // separated by exactly one blank line rather than three.
    expect(
      commandOutput({ ...ran, stdout: "one\n", stderr: "boom\n" }),
    ).toBe("$ stdout\none\n\n$ stderr\nboom");
  });

  it("says nothing at all when nothing was captured", () => {
    expect(commandOutput(ran)).toBeNull();
    expect(commandOutput(null)).toBeNull();
  });

  it("says so when the provider stopped capturing", () => {
    expect(
      commandOutput({ ...ran, stdout: "one\n", outputTruncated: true }),
    ).toContain("# output was truncated at the capture limit");
  });

  it("states the outcome alongside the command", () => {
    const detail = (result: Parameters<typeof commandOutput>[0]) =>
      toolPreviewPresentation(preview, result).detail;

    expect(detail({ ...ran, exitCode: 2 })).toContain("# exit code: 2");
    expect(detail({ ...ran, exitCode: null, timedOut: true })).toContain(
      "# stopped at the time limit",
    );
    expect(detail({ ...ran, exitCode: null })).toContain(
      "# killed by a signal",
    );
    // Before it has run there is no outcome to state.
    expect(detail(null)).toBe(
      "cargo test --workspace\n# working directory: checkout",
    );
  });

  it("says it is waiting rather than showing an empty pane mid-run", () => {
    const markup = renderToStaticMarkup(
      <ToolCommandCard
        name="exec"
        status="running"
        preview={preview}
        result={null}
      />,
    );

    expect(markup).toContain('role="tab"');
    expect(visibleText(markup)).toContain("Waiting for output…");
  });
});

describe("outcome badges", () => {
  const ran: ExecResultPreview = {
    tool: "exec",
    exitCode: 0,
    timedOut: false,
    outputTruncated: false,
    stdout: "",
    stderr: "",
  };

  it("prefers the most specific thing it can say about a failure", () => {
    const badge = (
      result: ExecResultPreview | null,
      status: "completed" | "failed",
    ) =>
      visibleText(
        renderToStaticMarkup(
          <ToolCommandCard
            name="exec"
            status={status}
            preview={preview}
            result={result}
          />,
        ),
      );

    expect(badge({ ...ran, exitCode: 101 }, "failed")).toContain("Exit 101");
    expect(badge({ ...ran, exitCode: null, timedOut: true }, "failed")).toContain(
      "Timed out",
    );
    expect(badge(ran, "completed")).toContain("Done");
    // No result to be specific about yet.
    expect(badge(null, "failed")).toContain("Failed");
  });
});
