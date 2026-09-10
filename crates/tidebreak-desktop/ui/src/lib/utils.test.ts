import { describe, expect, it } from "vitest";

import { HttpError } from "../api/client/http";
import { friendlyErrorMessage } from "./utils";

describe("friendlyErrorMessage", () => {
  it("yields the server message from an HttpError, without status prefix", () => {
    expect(
      friendlyErrorMessage(
        new HttpError(409, "409: repository already registered", "conflict"),
        "Could not add repository",
      ),
    ).toBe("repository already registered");
  });

  it("strips only the client's own status prefix, never digits the server wrote", () => {
    expect(
      friendlyErrorMessage(
        new HttpError(500, "500: 404: file not found", "internal"),
        "fallback",
      ),
    ).toBe("404: file not found");
  });

  it("yields a plain Error's message", () => {
    expect(friendlyErrorMessage(new Error("disk full"), "Could not save")).toBe(
      "disk full",
    );
  });

  it("yields a string as itself", () => {
    expect(friendlyErrorMessage("network down", "Could not reach")).toBe(
      "network down",
    );
  });

  it("falls back when the input is empty", () => {
    expect(friendlyErrorMessage("", "Could not complete")).toBe(
      "Could not complete",
    );
    expect(friendlyErrorMessage(new Error("   "), "Could not complete")).toBe(
      "Could not complete",
    );
  });

  it("falls back when an unknown shape is longer than 240 characters", () => {
    const long = "x".repeat(241);
    expect(friendlyErrorMessage(long, "Could not complete")).toBe(
      "Could not complete",
    );
  });

  it("keeps long HttpError detail instead of the fallback", () => {
    const detail = `git push failed:\n${"remote rejected\n".repeat(20).trimEnd()}`;
    expect(
      friendlyErrorMessage(
        new HttpError(409, `409: ${detail}`, "git_push_failed"),
        "Could not push",
      ),
    ).toBe(detail);
  });
});
