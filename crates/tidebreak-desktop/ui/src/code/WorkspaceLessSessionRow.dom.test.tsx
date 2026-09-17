// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";

import type { CodeSessionDigest } from "../api/types";
import { WorkspaceLessSessionRow } from "./WorkspaceLessSessionRow";
import { nestSessionDigests } from "./sessionTree";

afterEach(() => {
  cleanup();
});

function makeDigest(overrides: Partial<CodeSessionDigest>): CodeSessionDigest {
  return {
    workspace: null,
    session: "s-1",
    kind: "interactive",
    lifecycle: "running",
    attention: { state: { type: "working" }, source: "lifecycle" },
    title: "Deploy question",
    turn_count: 2,
    ...overrides,
  };
}

it("shows the engine and age beside the status, like a workspace card", () => {
  const twoDaysAgo = new Date(Date.now() - 2 * 24 * 3600 * 1000).toISOString();
  render(
    <WorkspaceLessSessionRow
      digest={makeDigest({
        harness_kind: "claude_code",
        trigger_target_at: twoDaysAgo,
        external_origin: {
          channel_kind: "slack",
          external_key: "T1/C42/171.2",
        },
      })}
      onOpen={() => {}}
    />,
  );
  const row = screen.getByRole("button", {
    name: "Deploy question, Slack channel, Agent working, Claude Code",
  });
  expect(row.querySelector("[title='Claude Code']")).not.toBeNull();
  expect(row.textContent).toContain("2d");
});

it("absorbs a modifier click instead of opening mid-selection", () => {
  const onOpen = vi.fn();
  render(<WorkspaceLessSessionRow digest={makeDigest({})} onOpen={onOpen} />);
  const row = screen.getByRole("button");
  fireEvent.click(row, { metaKey: true });
  fireEvent.click(row, { shiftKey: true });
  expect(onOpen).not.toHaveBeenCalled();
  fireEvent.click(row);
  expect(onOpen).toHaveBeenCalledWith("s-1");
});

it("renders and opens grandchildren once within bounded nesting", () => {
  const rows = [
    makeDigest({ session: "parent", title: "Parent" }),
    makeDigest({ session: "child", parent_session: "parent", title: "Child" }),
    makeDigest({
      session: "grandchild",
      parent_session: "child",
      title: "Grandchild",
    }),
    makeDigest({
      session: "overflow",
      parent_session: "grandchild",
      title: "Overflow",
    }),
  ];
  const { roots, childrenOf } = nestSessionDigests(rows);
  const onOpen = vi.fn();
  render(
    <>
      {roots.map((digest) => (
        <WorkspaceLessSessionRow
          key={digest.session}
          digest={digest}
          nested={childrenOf.get(digest.session)}
          childrenByParent={childrenOf}
          activeSessionId="grandchild"
          onOpen={onOpen}
        />
      ))}
    </>,
  );
  expect(screen.getAllByRole("button")).toHaveLength(4);
  const grandchild = screen.getByRole("button", { name: /^Grandchild,/ });
  expect(grandchild.getAttribute("aria-current")).toBe("page");
  fireEvent.click(grandchild);
  expect(onOpen).toHaveBeenCalledExactlyOnceWith("grandchild");
});
