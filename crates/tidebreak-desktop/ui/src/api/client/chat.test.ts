import { afterEach, describe, expect, it, vi } from "vitest";
import { withChatApi } from "./chat";
import { HttpCore } from "./http";

const Client = withChatApi(HttpCore);

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("markNotificationsRead", () => {
  it("posts the ids as JSON", async () => {
    // The server's JSON extractor answers 400 to a body sent without this
    // header, and the notifications stay unread.
    const fetch = vi.fn(
      async (_url: string, _init: RequestInit) =>
        new Response(JSON.stringify({ marked: 1 }), { status: 200 }),
    );
    vi.stubGlobal("fetch", fetch);

    const marked = await new Client(
      "https://machine.example",
      "token",
    ).markNotificationsRead(["n1", "n2"]);

    expect(marked).toBe(1);
    const [url, init] = fetch.mock.calls[0];
    expect(url).toBe("https://machine.example/notifications/read");
    expect(init.method).toBe("POST");
    expect(new Headers(init.headers).get("Content-Type")).toBe(
      "application/json",
    );
    expect(JSON.parse(String(init.body))).toEqual({ ids: ["n1", "n2"] });
  });
});
