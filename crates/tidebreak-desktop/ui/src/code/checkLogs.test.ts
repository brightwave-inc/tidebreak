import { describe, expect, it, vi } from "vitest";
import { toast } from "sonner";

import type { ApiClient } from "../api/client";
import { fetchFixErrorsLogs } from "./checkLogs";

vi.mock("sonner", () => ({
  toast: { message: vi.fn(), success: vi.fn(), error: vi.fn() },
}));

function client(
  writeCodeCheckLogs: ApiClient["writeCodeCheckLogs"],
): Pick<ApiClient, "writeCodeCheckLogs"> {
  return { writeCodeCheckLogs };
}

const log = {
  check: "clippy",
  path: "/data/code/private/ws-1/ci-logs/clippy-1.log",
  byte_len: 2048,
  truncated: false,
  url: "https://github.com/example/app/actions/runs/7/job/9",
};

describe("fetchFixErrorsLogs", () => {
  it("returns the downloaded logs", async () => {
    const logs = await fetchFixErrorsLogs(
      client(async () => ({ head_sha: "abc123", logs: [log], errors: [] })),
      "ws-1",
    );
    expect(logs).toEqual([log]);
  });

  /**
   * A GitHub outage must not disable the shortcut. The reader pressed Fix
   * errors; they get the turn they asked for, told once that the files are
   * missing.
   */
  it("says so and keeps going when the download fails", async () => {
    const logs = await fetchFixErrorsLogs(
      client(async () => {
        throw new Error("gh is signed out");
      }),
      "ws-1",
    );
    expect(logs).toEqual([]);
    expect(toast.message).toHaveBeenCalledWith(
      "Could not download the failing job logs",
      expect.objectContaining({ description: "gh is signed out" }),
    );
  });

  it("keeps the logs it did get when one job fails", async () => {
    const logs = await fetchFixErrorsLogs(
      client(async () => ({
        logs: [log],
        errors: [{ check: "desktop UI", message: "HTTP 404" }],
      })),
      "ws-1",
    );
    expect(logs).toEqual([log]);
    expect(toast.message).toHaveBeenCalledWith(
      "Could not download 1 of the failing job logs",
      expect.objectContaining({ description: "HTTP 404" }),
    );
  });
});
