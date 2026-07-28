import { describe, expect, it, vi } from "vitest";

import type { Chat } from "./api";
import { DERIVED_TITLE_LOOKUPS, lookUpDerivedTitle } from "./DerivedChatTitle";

function chat(title: string | null): Chat {
  return {
    id: "chat-1",
    project_id: null,
    title,
    model: null,
    reasoning_effort: null,
    attachment_revision: 0,
    root_attachments: [],
    created_at: "2026-07-28T12:00:00Z",
  };
}

/** Immediate, so the retry delay does not cost the suite two seconds. */
const noWait = () => Promise.resolve();

describe("adopting a derived chat title", () => {
  it("takes the name the server derived for an unnamed chat", async () => {
    const named = chat("Q3 revenue reconciliation");
    const getChat = vi.fn(async () => named);

    expect(
      await lookUpDerivedTitle({ getChat }, "chat-1", () => chat(null), noWait),
    ).toEqual(named);
    expect(getChat).toHaveBeenCalledTimes(1);
  });

  it("never asks about a chat that already has a name", async () => {
    const getChat = vi.fn(async () => chat("Derived"));

    expect(
      await lookUpDerivedTitle(
        { getChat },
        "chat-1",
        () => chat("Ledger work"),
        noWait,
      ),
    ).toBeNull();
    expect(getChat).not.toHaveBeenCalled();
  });

  it("looks again once for a turn that finished before the title did", async () => {
    const named = chat("Q3 revenue reconciliation");
    const getChat = vi
      .fn()
      .mockResolvedValueOnce(chat(null))
      .mockResolvedValueOnce(named);

    expect(
      await lookUpDerivedTitle({ getChat }, "chat-1", () => chat(null), noWait),
    ).toEqual(named);
    expect(getChat).toHaveBeenCalledTimes(DERIVED_TITLE_LOOKUPS.length);
  });

  it("stops looking rather than polling a chat the model declined to name", async () => {
    const getChat = vi.fn(async () => chat(null));

    expect(
      await lookUpDerivedTitle({ getChat }, "chat-1", () => chat(null), noWait),
    ).toBeNull();
    expect(getChat).toHaveBeenCalledTimes(DERIVED_TITLE_LOOKUPS.length);
  });
});
