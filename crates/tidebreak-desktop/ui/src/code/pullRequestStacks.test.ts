import { describe, expect, it } from "vitest";

import type { CodeDeliveryPullRequestSummary } from "../api/types";
import { arrangeStackLanes } from "./pullRequestStacks";

function summary(
  number: number,
  overrides: Partial<CodeDeliveryPullRequestSummary> = {},
): CodeDeliveryPullRequestSummary {
  return {
    id: `github.com/acme/tools#${number}`,
    repository: {
      host: "github.com",
      owner: "acme",
      name: "tools",
      name_with_owner: "acme/tools",
      url: "https://github.com/acme/tools",
    },
    number,
    url: `https://github.com/acme/tools/pull/${number}`,
    title: `PR ${number}`,
    state: "open",
    draft: false,
    head_branch: `feat/${number}`,
    base_branch: "main",
    auto_merge_enabled: false,
    checks: [],
    attention_reasons: [],
    ready_to_merge: false,
    workspace_links: [],
    labels: [],
    created_at: "2026-08-22T10:00:00Z",
    updated_at: "2026-08-22T10:00:00Z",
    ...overrides,
  } as CodeDeliveryPullRequestSummary;
}

describe("arrangeStackLanes", () => {
  it("nests children under their parent and keeps root order", () => {
    const rows = arrangeStackLanes([
      summary(3, { stack_parent_number: 2 }),
      summary(9),
      summary(1),
      summary(2, { stack_parent_number: 1 }),
    ]);
    expect(rows.map((row) => [row.item.number, row.depth])).toEqual([
      [9, 0],
      [1, 0],
      [2, 1],
      [3, 2],
    ]);
    expect(rows.every((row) => row.stackedOn === undefined)).toBe(true);
  });

  it("keeps a child whose parent is off the page at depth 0 with the badge", () => {
    const rows = arrangeStackLanes([summary(3, { stack_parent_number: 2 })]);
    expect(rows).toHaveLength(1);
    expect(rows[0].depth).toBe(0);
    expect(rows[0].stackedOn).toBe(2);
  });

  it("never links across repositories on a shared number", () => {
    const other = summary(2, { stack_parent_number: 1 });
    other.id = "github.com/acme/design-tokens#2";
    other.repository = {
      ...other.repository,
      name: "design-tokens",
      name_with_owner: "acme/design-tokens",
    };
    const rows = arrangeStackLanes([summary(1), other]);
    const child = rows.find((row) => row.item.number === 2);
    expect(child?.depth).toBe(0);
    expect(child?.stackedOn).toBe(1);
  });

  it("breaks a cycle instead of dropping or looping", () => {
    const rows = arrangeStackLanes([
      summary(1, { stack_parent_number: 2 }),
      summary(2, { stack_parent_number: 1 }),
    ]);
    expect(rows).toHaveLength(2);
    expect(rows[0].depth).toBe(0);
    expect(rows[0].stackedOn).toBeDefined();
  });

  it("caps indentation on an absurd chain", () => {
    const items = [summary(1)];
    for (let number = 2; number <= 15; number += 1) {
      items.push(summary(number, { stack_parent_number: number - 1 }));
    }
    const rows = arrangeStackLanes(items);
    expect(rows).toHaveLength(15);
    expect(Math.max(...rows.map((row) => row.depth))).toBe(10);
  });
});
