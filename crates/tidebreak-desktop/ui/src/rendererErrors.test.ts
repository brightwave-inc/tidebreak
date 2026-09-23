// @vitest-environment jsdom
import { describe, expect, it, vi } from "vitest";

import { createRendererErrorReporter, scrubLogText } from "./rendererErrors";

/**
 * Credential-shaped inputs, each assembled at run time so that no source line
 * holds one for a secret scanner to flag. The server's scrub tests build the
 * same values the same way.
 */
const FAKE = {
  launchBearer: ["launch", "bearer"].join("-"),
  userinfo: ["person:", "hunter", "2"].join(""),
  bearer: ["tb-", "launch-", "0123456789"].join(""),
  password: ["hunter", "2"].join(""),
  apiKey: ["abc", "123"].join(""),
  vendorKey: ["sk-", "ant-", "api03-", "abcdefghijklmnop"].join(""),
  githubToken: ["ghp", "0123456789abcdefghij"].join("_"),
  tidebreakToken: ["tidebreak-", "token", ".", "0123456789abcdef"].join(""),
  webToken: [
    "eyJhbG",
    "ciOiJIUzI1NiJ9",
    ".",
    "eyJzdW",
    "IiOiIxIn0",
    ".",
    "c2lnbm",
    "F0dXJl",
  ].join(""),
};

const LOCAL = {
  baseUrl: "http://127.0.0.1:4321/",
  token: FAKE.launchBearer,
  attachment: "local" as const,
};

function reporter(now = () => 0) {
  const fetch = vi.fn().mockResolvedValue(new Response(null, { status: 204 }));
  return {
    fetch,
    errors: createRendererErrorReporter({
      fetch: fetch as unknown as typeof globalThis.fetch,
      now,
    }),
  };
}

function sentBodies(fetch: ReturnType<typeof vi.fn>) {
  return fetch.mock.calls.map(([, init]) =>
    JSON.parse((init as RequestInit).body as string),
  );
}

describe("renderer error reporting", () => {
  it("holds errors until boot knows the server, then sends them with the bearer", () => {
    const { fetch, errors } = reporter();
    errors.report("render", new TypeError("row exploded"), {
      componentStack: "\n    at Row",
    });
    expect(fetch).not.toHaveBeenCalled();

    errors.connect(LOCAL);

    expect(fetch).toHaveBeenCalledOnce();
    const [url, init] = fetch.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("http://127.0.0.1:4321/diagnostics/renderer-errors");
    expect(init.method).toBe("POST");
    expect(init.headers).toMatchObject({
      Authorization: `Bearer ${LOCAL.token}`,
      "Content-Type": "application/json",
    });
    const [body] = sentBodies(fetch);
    expect(body).toMatchObject({
      kind: "render",
      message: "TypeError: row exploded",
      component_stack: "\n    at Row",
    });
    expect(body.stack).toContain("row exploded");
  });

  it("sends nothing while the window works on another machine", () => {
    const { fetch, errors } = reporter();
    errors.report("error", new Error("before boot"));
    errors.connect({ ...LOCAL, attachment: "remote" });
    errors.report("error", new Error("after boot"));
    expect(fetch).not.toHaveBeenCalled();
  });

  it("sends an error that repeats once a minute, not once a frame", () => {
    let clock = 0;
    const { fetch, errors } = reporter(() => clock);
    errors.connect(LOCAL);
    const repeat = new Error("same every frame");
    for (let frame = 0; frame < 30; frame += 1) {
      errors.report("render", repeat);
      clock += 16;
    }
    expect(fetch).toHaveBeenCalledOnce();

    clock += 60_000;
    errors.report("render", repeat);
    expect(fetch).toHaveBeenCalledTimes(2);
  });

  it("stops at a fixed number of reports per page load", () => {
    const { fetch, errors } = reporter();
    errors.connect(LOCAL);
    for (let index = 0; index < 80; index += 1) {
      errors.report("error", new Error(`distinct ${index}`));
    }
    expect(fetch).toHaveBeenCalledTimes(50);
  });

  it("reports the window's uncaught errors and unhandled rejections", () => {
    const { fetch, errors } = reporter();
    errors.connect(LOCAL);
    const target = new EventTarget() as unknown as Window;
    const uninstall = errors.install(target);

    const thrown = new Error("uncaught in a timer");
    target.dispatchEvent(
      new ErrorEvent("error", {
        error: thrown,
        message: thrown.message,
        filename: "http://tauri.localhost/assets/index.js",
        lineno: 12,
        colno: 7,
      }),
    );
    const rejection = new Event("unhandledrejection") as PromiseRejectionEvent;
    Object.defineProperty(rejection, "reason", { value: "socket closed" });
    target.dispatchEvent(rejection);
    uninstall();
    target.dispatchEvent(new ErrorEvent("error", { message: "after" }));

    expect(sentBodies(fetch)).toEqual([
      expect.objectContaining({
        kind: "error",
        message: "Error: uncaught in a timer",
        source: "http://tauri.localhost/assets/index.js",
        line: 12,
        column: 7,
      }),
      { kind: "unhandled_rejection", message: "socket closed" },
    ]);
  });

  /** The launch bearer has no prefix the scrub knows, so it goes by value. */
  it("keeps the launch bearer out of the report", () => {
    const { fetch, errors } = reporter();
    errors.connect(LOCAL);
    errors.report(
      "unhandled_rejection",
      new Error(`handshake refused for ${LOCAL.token}`),
    );
    const [body] = sentBodies(fetch);
    expect(JSON.stringify(body)).not.toContain(LOCAL.token);
    expect(body.message).toBe("Error: handshake refused for [token]");
  });

  /** A rejection's values used to reach the log as full JSON. */
  it("names a rejected object by its keys and scrubs what it keeps", () => {
    const { fetch, errors } = reporter();
    errors.connect(LOCAL);
    const target = new EventTarget() as unknown as Window;
    errors.install(target);

    const withValues = new Event("unhandledrejection") as PromiseRejectionEvent;
    Object.defineProperty(withValues, "reason", {
      value: {
        prompt: "draft the quarterly letter to investors",
        token: FAKE.githubToken,
        url: "https://api.example.com/v1/chats?draft=hello&key=abc",
      },
    });
    target.dispatchEvent(withValues);
    const withMessage = new Event(
      "unhandledrejection",
    ) as PromiseRejectionEvent;
    Object.defineProperty(withMessage, "reason", {
      value: {
        message: `GET https://api.example.com/v1/chats?draft=hello failed: Authorization: Bearer ${FAKE.vendorKey}`,
      },
    });
    target.dispatchEvent(withMessage);

    const [values, message] = sentBodies(fetch);
    expect(values.message).toBe(
      "Object with keys prompt, token, url (not an Error)",
    );
    expect(message.message).toBe(
      "GET https://api.example.com/v1/chats?[redacted] failed: Authorization: Bearer [redacted]",
    );
    const sent = JSON.stringify(sentBodies(fetch));
    for (const secret of [
      "quarterly letter",
      "ghp_",
      "draft=hello",
      "sk-ant",
    ]) {
      expect(sent).not.toContain(secret);
    }
  });

  it("scrubs stacks and cuts long values short", () => {
    const { fetch, errors } = reporter();
    errors.connect(LOCAL);
    const error = new Error("x".repeat(5_000));
    error.stack =
      "Error: boom\n    at load (http://127.0.0.1:4321/assets/app.js?session=abc:1:2)";
    errors.report("error", error, {
      source: "http://127.0.0.1:4321/assets/app.js?session=abc",
    });

    const [body] = sentBodies(fetch);
    expect(Array.from(body.message as string)).toHaveLength(1_001);
    expect(body.message.endsWith("…")).toBe(true);
    expect(body.stack).toBe(
      "Error: boom\n    at load (http://127.0.0.1:4321/assets/app.js?[redacted])",
    );
    expect(body.source).toBe("http://127.0.0.1:4321/assets/app.js?[redacted]");
  });

  /** The same cases as the server's `scrubbing_removes_queries_credentials_and_tokens`. */
  it("scrubs text by the same rules as the server's log", () => {
    const cases: [string, string][] = [
      [
        "fetch https://api.example.com/v1/chats?token=abc&q=my+prompt#frag failed",
        "fetch https://api.example.com/v1/chats?[redacted] failed",
      ],
      [
        "at (http://127.0.0.1:4321/chats/7?draft=hello)",
        "at (http://127.0.0.1:4321/chats/7?[redacted])",
      ],
      [
        `clone https://${FAKE.userinfo}@github.com/o/r.git`,
        "clone https://[redacted]@github.com/o/r.git",
      ],
      [
        "GET /sessions/9/events?cursor=4&key=x",
        "GET /sessions/9/events?[redacted]",
      ],
      [
        `Authorization: Bearer ${FAKE.bearer} sent`,
        "Authorization: Bearer [redacted] sent",
      ],
      [`password=${FAKE.password} user=ada`, "password=[redacted] user=ada"],
      [
        "the session token expired; sign in again",
        "the session token expired; sign in again",
      ],
      [`{"api_key":"${FAKE.apiKey}","model":"m"}`, '{"api_key":[redacted]'],
      [`provider said ${FAKE.vendorKey}`, "provider said [redacted]"],
      [`subprotocol ${FAKE.tidebreakToken}`, "subprotocol [redacted]"],
      [`jwt ${FAKE.webToken}`, "jwt [redacted]"],
      [
        "index out of bounds: the len is 3 but the index is 7",
        "index out of bounds: the len is 3 but the index is 7",
      ],
      ["a task-list and sk-small stay", "a task-list and sk-small stay"],
    ];
    for (const [input, expected] of cases) {
      expect(scrubLogText(input, 500)).toBe(expected);
    }
    const long = `first line\n${"x".repeat(2_000)}`;
    const kept = scrubLogText(long, 100);
    expect(kept.startsWith("first line\nxxx")).toBe(true);
    expect(Array.from(kept)).toHaveLength(101);
  });

  it("never throws, even when sending fails", () => {
    const fetch = vi.fn(() => {
      throw new Error("fetch unavailable");
    });
    const errors = createRendererErrorReporter({
      fetch: fetch as unknown as typeof globalThis.fetch,
    });
    errors.connect(LOCAL);
    expect(() => errors.report("error", new Error("boom"))).not.toThrow();
  });
});
