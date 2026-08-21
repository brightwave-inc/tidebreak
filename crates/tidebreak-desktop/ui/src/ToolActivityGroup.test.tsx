import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import {
  ToolActivityGroup,
  toolActivityGroupPresentation,
} from "./ToolActivityGroup";

function labelOf(
  activities: Parameters<typeof toolActivityGroupPresentation>[0],
) {
  return toolActivityGroupPresentation(activities).label;
}

describe("toolActivityGroupPresentation", () => {
  it("names the latest thing that happened and counts the rest", () => {
    expect(
      labelOf([
        { name: "search", status: "completed" },
        { name: "read_file", status: "completed" },
        { name: "web_search", status: "completed" },
      ]),
    ).toBe("Searched the web and 2 other tasks");
  });

  it("keeps the whole phase in the present while any part of it is live", () => {
    // The lead reads as in-progress even though it has settled, so the line
    // doesn't flicker between tenses as calls finish underneath it.
    const presentation = toolActivityGroupPresentation([
      { name: "list_dir", status: "running" },
      { name: "read_file", status: "completed" },
    ]);

    expect(presentation.label).toBe("Reading a file and 1 other task");
    expect(presentation.phase).toBe("active");
    expect(presentation.inProgress).toBe(true);
  });

  it("narrows a live phase label to the latest assistant snapshot", () => {
    const activities = [
      { name: "web_search", status: "running" as const },
      { name: "read_file", status: "completed" as const },
    ];
    const presentation = toolActivityGroupPresentation(activities, [
      activities[0]!,
    ]);

    expect(presentation.label).toBe("Searching the web");
    expect(presentation.phase).toBe("active");
  });

  it("aggregates delegation by count and waiting without one", () => {
    expect(
      labelOf([
        { name: "spawn_sandbox_agent", status: "completed" },
        { name: "spawn_sandbox_agent", status: "completed" },
      ]),
    ).toBe("Delegated 2 tasks");

    // How many agents are being waited on is not what the line is about.
    expect(
      labelOf([
        { name: "spawn_sandbox_agent", status: "completed" },
        { name: "wait_for_agents", status: "running" },
      ]),
    ).toBe("Waiting for background agents and delegating 1 task");
  });

  it("mixes categories in a readable order", () => {
    expect(
      labelOf([
        { name: "spawn_sandbox_agent", status: "completed" },
        { name: "read_file", status: "completed" },
        { name: "search", status: "completed" },
        { name: "wait_for_agents", status: "completed" },
        { name: "web_search", status: "completed" },
      ]),
    ).toBe(
      "Searched the web, waited for background agents, delegated 1 task, and 2 other tasks",
    );
  });

  it("reports the worst terminal outcome without naming the tools that failed", () => {
    const presentation = toolActivityGroupPresentation([
      { name: "web_search", status: "completed" },
      { name: "write_file", status: "failed" },
      { name: "list_dir", status: "cancelled" },
    ]);

    expect(presentation.tone).toBe("failed");
    expect(presentation.phase).toBe("settled");
  });

  it("degrades an empty phase without throwing", () => {
    expect(toolActivityGroupPresentation([])).toMatchObject({
      tone: "unknown",
      label: "Tool activity unavailable",
    });
  });
});

describe("ToolActivityGroup", () => {
  it("collapses the whole phase behind one line", () => {
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
    expect(markup).toContain("Read a file and 1 other task");
    // Collapsed means absent, not hidden: nothing to read past.
    expect(markup).not.toContain('role="listitem"');
  });

  it("keeps surfaced cards outside the collapsed region", () => {
    const markup = renderToStaticMarkup(
      <ToolActivityGroup
        groupIndex={0}
        activities={[{ name: "exec", status: "completed" }]}
      >
        <p>a card that must stay reachable</p>
      </ToolActivityGroup>,
    );

    expect(markup).toContain("a card that must stay reachable");
  });

  it("degrades a malformed activity list to fixed copy", () => {
    const markup = renderToStaticMarkup(
      <ToolActivityGroup
        groupIndex={0}
        activities={[{ name: "web_search" }, null, "private"] as never}
      />,
    );

    expect(markup).toContain("Tool activity unavailable");
    expect(markup).not.toContain("private");
  });
});
