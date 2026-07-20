import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import type { AgentRun } from "./api";
import { AgentActivityPanel, agentRunsForChat } from "./AgentActivityPanel";

function run(overrides: Partial<AgentRun>): AgentRun {
  return {
    id: "run-1",
    parent_id: null,
    execution: "sandbox",
    status: "running",
    started_at: "2026-07-19T12:00:00Z",
    finished_at: null,
    last_error_code: null,
    activity: null,
    created_at: "2026-07-19T12:00:00Z",
    updated_at: "2026-07-19T12:00:00Z",
    ...overrides,
  };
}

const noStops = {
  stoppingRunIds: new Set<string>(),
  stopErrorRunIds: new Set<string>(),
  onStop: vi.fn(),
};

describe("AgentActivityPanel", () => {
  it("shows snapshots only for the conversation that owns them", () => {
    const runs = [run({ id: "chat-a-run" })];

    expect(agentRunsForChat("chat-a", "chat-a", runs)).toBe(runs);
    expect(agentRunsForChat("chat-a", "chat-b", runs)).toEqual([]);
    expect(agentRunsForChat(null, "chat-a", runs)).toEqual([]);
  });

  it("groups live and recent background work with a focused live region", () => {
    const markup = renderToStaticMarkup(
      <AgentActivityPanel
        runs={[
          run({ id: "foreground", execution: "foreground", status: "active" }),
          run({ id: "live", status: "running" }),
          run({
            id: "search",
            status: "waiting",
            activity: { kind: "web_search", status: "running" },
          }),
          run({ id: "complete", status: "completed", updated_at: "2026-07-19T12:03:00Z" }),
          run({ id: "failed", status: "failed", updated_at: "2026-07-19T12:02:00Z" }),
          run({ id: "old", status: "cancelled", updated_at: "2026-07-19T12:01:00Z" }),
        ]}
        loading={false}
        error={null}
        onRetry={vi.fn()}
        {...noStops}
      />,
    );

    expect(markup).toContain('aria-label="Active background tasks"');
    expect(markup).toContain('aria-label="Recent background tasks"');
    expect(markup).toContain('aria-live="polite"');
    expect(markup).toContain(
      '<strong aria-live="polite" aria-atomic="true" aria-label="Background task 1: running">running</strong>',
    );
    expect(markup).not.toContain('aria-label="Conversation: active"');
    expect(markup).toContain("2 background tasks");
    expect(markup).toContain("Searching the web");
    expect(markup).toContain("1 earlier result");
    expect(markup).not.toContain('aria-live="polite" aria-label="Agent activity"');
  });

  it("renders only generic safe copy instead of run identifiers or diagnostics", () => {
    const markup = renderToStaticMarkup(
      <AgentActivityPanel
        runs={[
          run({
            id: "private-run-identity",
            parent_id: "private-parent-identity",
            status: "failed",
            last_error_code: "private_provider_diagnostic",
            activity: { kind: "read_connected_file", status: "running" },
          }),
        ]}
        loading={false}
        error={null}
        onRetry={vi.fn()}
        {...noStops}
      />,
    );

    expect(markup).toContain("Background task 1");
    expect(markup).toContain("Could not finish");
    expect(markup).not.toContain("private-run-identity");
    expect(markup).not.toContain("private-parent-identity");
    expect(markup).not.toContain("private_provider_diagnostic");
    expect(markup).not.toContain("Reading a file");
  });

  it("falls back safely for malformed future activity projections", () => {
    const malformedRuns = [
      run({
        id: "unknown-kind",
        activity: { kind: "future_private_tool", status: "running" } as never,
      }),
      run({
        id: "unknown-status",
        activity: { kind: "web_search", status: "future_status" } as never,
      }),
    ];

    const markup = renderToStaticMarkup(
      <AgentActivityPanel
        runs={malformedRuns}
        loading={false}
        error={null}
        onRetry={vi.fn()}
        {...noStops}
      />,
    );

    expect(markup).toContain("Background task 1");
    expect(markup).toContain("Background task 2");
    expect(markup).toContain("Working in the background");
    expect(markup).not.toContain("future_private_tool");
    expect(markup).not.toContain("future_status");
  });

  it("keeps load and retry states compact and accessible", () => {
    expect(
      renderToStaticMarkup(
        <AgentActivityPanel
          runs={[]}
          loading
          error={null}
          onRetry={vi.fn()}
          {...noStops}
        />,
      ),
    ).toContain("Loading activity…");

    const failure = renderToStaticMarkup(
      <AgentActivityPanel
        runs={[]}
        loading={false}
        error="private network detail"
        onRetry={vi.fn()}
        {...noStops}
      />,
    );
    expect(failure).toContain('role="status"');
    expect(failure).toContain("Activity unavailable");
    expect(failure).toContain("Retry");
    expect(failure).not.toContain("private network detail");
  });

  it("offers exact active sandbox stops with safe pending and error copy", () => {
    const markup = renderToStaticMarkup(
      <AgentActivityPanel
        runs={[
          run({ id: "running" }),
          run({ id: "pending", status: "waiting" }),
          run({ id: "stopping", status: "cancelling" }),
          run({ id: "recent", status: "completed" }),
          run({ id: "foreground", execution: "foreground", status: "active" }),
        ]}
        loading={false}
        error={null}
        onRetry={vi.fn()}
        stoppingRunIds={new Set(["pending"])}
        stopErrorRunIds={new Set(["running"])}
        onStop={vi.fn()}
      />,
    );

    expect(markup.match(/>Stop<\/button>/g)).toHaveLength(1);
    expect(markup).toContain("Stopping…");
    expect(markup).toContain("disabled=\"\"");
    expect(markup).toContain('role="status"');
    expect(markup).toContain("Could not stop this task. Try again.");
    expect(markup).not.toContain("private");
  });

  it("suppresses an ambiguous request error once polling confirms stopping", () => {
    const markup = renderToStaticMarkup(
      <AgentActivityPanel
        runs={[run({ id: "stopping", status: "cancelling" })]}
        loading={false}
        error={null}
        onRetry={vi.fn()}
        stoppingRunIds={new Set()}
        stopErrorRunIds={new Set(["stopping"])}
        onStop={vi.fn()}
      />,
    );

    expect(markup).toContain("Stopping");
    expect(markup).not.toContain("Could not stop");
    expect(markup).not.toContain(">Stop</button>");
  });
});
