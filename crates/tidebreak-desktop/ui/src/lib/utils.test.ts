import { describe, expect, it } from "vitest";

import { HttpError } from "../api/client/http";
import {
  friendlyErrorMessage,
  SERVER_TIMEOUT_MESSAGE,
  SERVER_UNAVAILABLE_MESSAGE,
  UNREACHABLE_SERVER_MESSAGE,
} from "./utils";

/** An `HttpError` the way `throwIfNotOk` builds it from a JSON error body. */
function serverError(status: number, message: string, kind?: string) {
  return new HttpError(status, `${status}: ${message}`, kind, {
    ...(kind ? { kind } : {}),
    message,
  });
}

describe("friendlyErrorMessage", () => {
  it("yields the server message from an HttpError, without status prefix", () => {
    expect(
      friendlyErrorMessage(
        serverError(409, "repository already registered", "conflict"),
        "Could not add repository",
      ),
    ).toBe("Repository already registered");
  });

  it("strips only the client's own status prefix, never digits the server wrote", () => {
    expect(
      friendlyErrorMessage(
        serverError(500, "404: file not found", "git_failed"),
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
        serverError(409, detail, "git_push_failed"),
        "Could not push",
      ),
    ).toBe(`G${detail.slice(1)}`);
  });

  it("words a request that never reached the server, in every engine's spelling", () => {
    for (const failure of [
      new TypeError("Load failed"),
      new TypeError("Failed to fetch"),
      new TypeError("NetworkError when attempting to fetch resource."),
      "TypeError: Load failed",
    ]) {
      expect(friendlyErrorMessage(failure, "Could not load work")).toBe(
        UNREACHABLE_SERVER_MESSAGE,
      );
    }
  });

  it("drops the HttpError name and status that String(err) would show", () => {
    const error = serverError(409, "approval already decided", "conflict");
    expect(String(error)).toBe("HttpError: 409: approval already decided");
    expect(friendlyErrorMessage(String(error), "fallback")).toBe(
      "Approval already decided",
    );
    expect(friendlyErrorMessage(error, "fallback")).toBe(
      "Approval already decided",
    );
    expect(friendlyErrorMessage(error, "fallback")).not.toMatch(
      /HttpError|409/,
    );
  });

  it("keeps the server's message for every kind that writes one for people", () => {
    // Each of these is a message the server sends today, under the kind it
    // sends it with. Canned copy in their place told the reader nothing.
    for (const [status, kind, message] of [
      [
        500,
        "internal",
        "OpenAI transcription failed with status 401 Unauthorized",
      ],
      [
        500,
        "internal",
        "local voice input is available only in the desktop app",
      ],
      [500, "internal", "code mode is not configured on this server"],
      [401, "unauthorized", "Sign in to save your subscription preference."],
      [404, "not_found", "No pull request exists for this branch."],
      [404, "not_found", "file not found: src/main.rs"],
      [
        409,
        "workspace_archived",
        "This workspace is archived. Its files come back when you restore it.",
      ],
    ] as const) {
      const shown = friendlyErrorMessage(
        serverError(status, message, kind),
        "fallback",
      );
      expect(shown.toLowerCase(), kind).toBe(message.toLowerCase());
      expect(shown, kind).not.toMatch(/^\d{3}:|HttpError/);
    }
  });

  it("uses renderer copy only for kinds whose text is written for a log", () => {
    for (const [kind, message, copy] of [
      ["store", "store error: database is locked (code 5)", /local data/],
      ["serde", "missing field `id` at line 1 column 2", /part of its data/],
      ["secret", "secret error: errSecInteractionNotAllowed", /credentials/],
    ] as const) {
      expect(
        friendlyErrorMessage(serverError(500, message, kind), "fallback"),
        kind,
      ).toMatch(copy);
    }
  });

  it("lets a caller's kind copy win over the shared copy and the server's text", () => {
    expect(
      friendlyErrorMessage(
        serverError(404, "app app_1 not found", "not_found"),
        "Could not load this app.",
        { not_found: "This app no longer exists." },
      ),
    ).toBe("This app no longer exists.");
  });

  it("says the server is unavailable for a bodiless 5xx rather than its status text", () => {
    expect(
      friendlyErrorMessage(
        new HttpError(503, "503: Service Unavailable"),
        "Could not load the document",
      ),
    ).toBe(SERVER_UNAVAILABLE_MESSAGE);
  });

  it("reads a stringified status-only HttpError the way it reads the error", () => {
    expect(
      friendlyErrorMessage("HttpError: 503: Service Unavailable", "fallback"),
    ).toBe(SERVER_UNAVAILABLE_MESSAGE);
    expect(
      friendlyErrorMessage("HttpError: 400: Bad Request", "fallback"),
    ).toBe("fallback");
    expect(friendlyErrorMessage("HttpError: 502: ", "fallback")).toBe(
      SERVER_UNAVAILABLE_MESSAGE,
    );
    // A reason phrase alone says nothing a reader can act on.
    expect(
      friendlyErrorMessage(new Error("Service Unavailable"), "fallback"),
    ).toBe("fallback");
  });

  it("falls back for a bodiless 4xx rather than showing its status text", () => {
    expect(
      friendlyErrorMessage(
        new HttpError(400, "400: Bad Request"),
        "Could not save",
      ),
    ).toBe("Could not save");
  });

  it("starts the server's log-style text as a sentence, and leaves identifiers alone", () => {
    expect(
      friendlyErrorMessage(
        serverError(409, "the project already holds that file", "conflict"),
        "fallback",
      ),
    ).toBe("The project already holds that file");
    for (const verbatim of [
      "api_key must not be empty",
      "npm install failed",
      "git: not a repository",
    ]) {
      expect(
        friendlyErrorMessage(serverError(400, verbatim, "bad_request"), "x"),
      ).toBe(verbatim);
    }
  });

  it("keeps a message the renderer wrote into a bodiless HttpError", () => {
    expect(
      friendlyErrorMessage(
        new HttpError(
          409,
          "409: Refresh the pull request before merging it.",
          "pr_identity_missing",
        ),
        "Could not merge",
      ),
    ).toBe("Refresh the pull request before merging it.");
  });

  it("words a timed-out request", () => {
    expect(
      friendlyErrorMessage(
        new DOMException("signal timed out", "TimeoutError"),
        "fallback",
      ),
    ).toBe(SERVER_TIMEOUT_MESSAGE);
  });
});
