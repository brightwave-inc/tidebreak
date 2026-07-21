import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import {
  ToolActivityGroup,
  toolActivityGroupPresentation,
  type ToolActivity,
} from "./ToolActivityGroup";

describe("ToolActivityGroup", () => {
  it("summarizes a small completed phase with semantic allowlisted copy", () => {
    expect(
      toolActivityGroupPresentation([
        { name: "web_search", status: "completed" },
        { name: "read_file", status: "completed" },
      ]),
    ).toEqual({
      phase: "settled",
      tone: "completed",
      icon: "✓",
      label: "Searched the web and read a file",
    });
  });

  it("summarizes mixed terminal outcomes without provider details", () => {
    expect(
      toolActivityGroupPresentation([
        { name: "web_search", status: "completed" },
        { name: "write_file", status: "failed" },
        { name: "list_dir", status: "cancelled" },
      ]),
    ).toEqual({
      phase: "settled",
      tone: "failed",
      icon: "!",
      label: "3 tool activities · 1 completed · 1 failed · 1 not run",
    });
  });

  it("keeps an active phase distinct from settled history", () => {
    expect(
      toolActivityGroupPresentation([
        { name: "read_file", status: "completed" },
        { name: "list_dir", status: "running" },
      ]),
    ).toEqual({
      phase: "active",
      tone: "running",
      icon: "↗",
      label: "Browsing files",
    });
  });

  it("uses the dedicated active copy while background agents are pending", () => {
    expect(
      toolActivityGroupPresentation([
        { name: "spawn_sandbox_agent", status: "completed" },
        { name: "wait_for_agents", status: "running" },
      ]),
    ).toEqual({
      phase: "active",
      tone: "running",
      icon: "↗",
      label: "Waiting for background agents",
    });
  });

  it("renders a native disclosure with a hidden ordered timeline", () => {
    const markup = renderToStaticMarkup(
      <ToolActivityGroup
        groupIndex={3}
        activities={[
          { name: "web_search", status: "completed" },
          { name: "read_file", status: "failed" },
        ]}
      />,
    );

    expect(markup).toContain('aria-expanded="false"');
    expect(markup).toContain('aria-controls="tool-activity-group-3"');
    expect(markup).toContain(
      'id="tool-activity-group-3" hidden="" class="tool-activity-group-list" role="list"',
    );
    expect(markup.match(/role="listitem"/g)).toHaveLength(2);
    expect(markup).not.toContain("tool-call-card");
    expect(markup).not.toContain('aria-live="polite"');
  });

  it("uses fixed fallback copy for unknown names and malformed statuses", () => {
    const markup = renderToStaticMarkup(
      <ToolActivityGroup
        groupIndex={0}
        activities={[
          {
            id: "private-call-id",
            name: "private_provider_tool_name",
            status: "private-provider-diagnostic" as "completed",
          },
        ]}
      />,
    );

    expect(markup).toContain("1 tool activity · 1 unavailable");
    expect(markup).toContain("Use a tool");
    expect(markup).toContain("Status unavailable");
    expect(markup).not.toContain("private_provider_tool_name");
    expect(markup).not.toContain("private-provider-diagnostic");
    expect(markup).not.toContain("private-call-id");
  });

  it("does not render a broken disclosure for an invalid activity list", () => {
    const markup = renderToStaticMarkup(
      <ToolActivityGroup
        groupIndex={0}
        activities={undefined as unknown as ToolActivity[]}
      />,
    );

    expect(markup).toContain("Tool activity unavailable");
    expect(markup).not.toContain("aria-controls");
    expect(markup).not.toContain("role=\"list\"");
  });
});
