import { afterEach, expect, it, vi } from "vitest";

const attachTrustedFolders = vi.hoisted(() => vi.fn());

vi.mock("../host", () => ({
  attachedRemotely: () => false,
  attachTrustedFolders,
}));

import { ApiClient } from "./client";

afterEach(() => {
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

it("attaches saved folders before returning a newly created chat", async () => {
  const chat = {
    id: "chat-1",
    title: null,
    model: null,
    attachment_revision: 0,
    root_attachments: [],
    project_id: null,
    created_at: "2026-08-25T12:00:00Z",
  };
  vi.stubGlobal(
    "fetch",
    vi.fn().mockResolvedValue(
      new Response(JSON.stringify(chat), {
        status: 201,
        headers: { "Content-Type": "application/json" },
      }),
    ),
  );
  attachTrustedFolders.mockResolvedValue([]);

  const created = await new ApiClient("http://127.0.0.1", "token").createChat();

  expect(attachTrustedFolders).toHaveBeenCalledWith(chat);
  expect(created).toEqual(chat);
});

it("returns the created chat when saved-folder attachment fails", async () => {
  const chat = {
    id: "chat-2",
    title: null,
    model: null,
    attachment_revision: 0,
    root_attachments: [],
    project_id: null,
    created_at: "2026-08-25T12:00:00Z",
  };
  vi.stubGlobal(
    "fetch",
    vi.fn().mockResolvedValue(
      new Response(JSON.stringify(chat), {
        status: 201,
        headers: { "Content-Type": "application/json" },
      }),
    ),
  );
  attachTrustedFolders.mockRejectedValue(new Error("broker unavailable"));
  const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);

  const created = await new ApiClient("http://127.0.0.1", "token").createChat();

  expect(created).toEqual(chat);
  expect(warn).toHaveBeenCalledWith(
    "could not attach saved folders to the new chat",
    expect.any(Error),
  );
  warn.mockRestore();
});
