import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import type { Project } from "./api";
import { ProjectNavigation } from "./ProjectNavigation";

function project(id: string, title: string | null): Project {
  return {
    id,
    title,
    attachment_revision: 0,
    root_attachments: [],
    created_at: "2026-07-21T12:00:00Z",
  };
}

describe("ProjectNavigation", () => {
  it("renders loose and project scopes with one selected scope", () => {
    const markup = renderToStaticMarkup(
      <ProjectNavigation
        projects={[project("project-a", "Research"), project("project-b", null)]}
        selectedProjectId="project-a"
        disabled={false}
        error={null}
        onSelect={vi.fn()}
        onCreate={vi.fn()}
      />,
    );

    expect(markup).toContain('aria-label="Projects"');
    expect(markup).toContain("Loose chats");
    expect(markup).toContain("Research");
    expect(markup).toContain("Untitled project");
    expect(markup).toContain('aria-current="page"');
    expect(markup.match(/aria-current="page"/g)).toHaveLength(1);
  });

  it("keeps project failures bounded to the sidebar", () => {
    const markup = renderToStaticMarkup(
      <ProjectNavigation
        projects={[]}
        selectedProjectId={null}
        disabled
        error="Could not load projects."
        onSelect={vi.fn()}
        onCreate={vi.fn()}
      />,
    );

    expect(markup).toContain("Could not load projects.");
    expect(markup).toContain("disabled");
  });
});
