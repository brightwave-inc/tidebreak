import { describe, expect, it } from "vitest";

import type { Chat } from "./api";
import {
  activityGroup,
  groupChats,
  hasListState,
  isListableChat,
  predatesListState,
  sortChats,
} from "./chatListGroups";

// A Wednesday afternoon, local time, so every group has a clear boundary.
const NOW = new Date(2026, 8, 23, 15, 30);

function at(daysAgo: number, hour = 10): string {
  return new Date(2026, 8, 23 - daysAgo, hour, 0).toISOString();
}

function chat(id: string, fields: Partial<Chat> = {}): Chat {
  return {
    id,
    project_id: null,
    title: id,
    model: null,
    reasoning_effort: null,
    permission_mode: null,
    network_policy: { mode: "off" },
    attachment_revision: 0,
    root_attachments: [],
    memory_incognito: false,
    created_at: at(40),
    last_activity_at: at(0),
    pinned_at: null,
    archived_at: null,
    running: false,
    unread: false,
    turn_count: 1,
    ...fields,
  };
}

describe("activityGroup", () => {
  it("reads the local calendar, not 24-hour windows", () => {
    expect(activityGroup(at(0, 0), NOW)).toBe("today");
    // Late last night is yesterday even though it was under a day ago.
    expect(activityGroup(at(1, 23), NOW)).toBe("yesterday");
    expect(activityGroup(at(1, 0), NOW)).toBe("yesterday");
    expect(activityGroup(at(2), NOW)).toBe("previous-7-days");
    expect(activityGroup(at(7), NOW)).toBe("previous-7-days");
    expect(activityGroup(at(8), NOW)).toBe("older");
  });
});

describe("groupChats", () => {
  it("cuts the list into pinned work and date groups, leaving out empty ones", () => {
    const groups = groupChats(
      [
        chat("older", { last_activity_at: at(30) }),
        chat("today", { last_activity_at: at(0) }),
        chat("pinned", { pinned_at: at(3), last_activity_at: at(20) }),
        chat("yesterday", { last_activity_at: at(1) }),
      ],
      NOW,
    );
    expect(
      groups.map((group) => [group.label, group.chats.map((c) => c.id)]),
    ).toEqual([
      ["Pinned", ["pinned"]],
      ["Today", ["today"]],
      ["Yesterday", ["yesterday"]],
      ["Older", ["older"]],
    ]);
  });

  it("orders each group by latest activity, whatever order it arrived in", () => {
    const [today] = groupChats(
      [
        chat("earlier", { last_activity_at: at(0, 9) }),
        chat("later", { last_activity_at: at(0, 14) }),
      ],
      NOW,
    );
    expect(today.chats.map((c) => c.id)).toEqual(["later", "earlier"]);
  });

  it("keeps the most recently pinned on top of the pinned group", () => {
    expect(
      sortChats([
        chat("first-pin", { pinned_at: at(5) }),
        chat("unpinned", { last_activity_at: at(0, 15) }),
        chat("latest-pin", { pinned_at: at(1), last_activity_at: at(9) }),
      ]).map((c) => c.id),
    ).toEqual(["latest-pin", "first-pin", "unpinned"]);
  });
});

describe("isListableChat", () => {
  it("lists a chat once a turn, a name, or a pin has happened", () => {
    const empty = chat("empty", { title: null, turn_count: 0 });
    expect(isListableChat(empty)).toBe(false);
    expect(isListableChat({ ...empty, turn_count: 1 })).toBe(true);
    expect(isListableChat({ ...empty, title: "Plan" })).toBe(true);
    expect(isListableChat({ ...empty, title: "   " })).toBe(false);
    expect(isListableChat({ ...empty, pinned_at: at(0) })).toBe(true);
    expect(isListableChat(empty, "empty")).toBe(true);
  });
});

describe("a server older than the list of work", () => {
  const LIST_FIELDS = new Set([
    "last_activity_at",
    "pinned_at",
    "archived_at",
    "running",
    "unread",
    "turn_count",
  ]);

  // The bare conversation such a server sends: none of the list fields.
  function bare(id: string, fields: Partial<Chat> = {}): Chat {
    return Object.fromEntries(
      Object.entries(chat(id, fields)).filter(([key]) => !LIST_FIELDS.has(key)),
    ) as Chat;
  }

  it("keeps every row, grouped by when it was created", () => {
    const rows = [
      bare("old", { created_at: at(30) }),
      bare("untitled", { title: null, created_at: at(0) }),
    ];
    expect(predatesListState(rows)).toBe(true);
    expect(hasListState(rows[0])).toBe(false);
    expect(rows.every((row) => isListableChat(row))).toBe(true);
    expect(
      groupChats(rows, NOW).map((group) => [
        group.label,
        group.chats.map((c) => c.id),
      ]),
    ).toEqual([
      ["Today", ["untitled"]],
      ["Older", ["old"]],
    ]);
  });

  it("cannot tell from an empty list, so it counts as current", () => {
    expect(predatesListState([])).toBe(false);
    expect(predatesListState([chat("current")])).toBe(false);
  });
});
