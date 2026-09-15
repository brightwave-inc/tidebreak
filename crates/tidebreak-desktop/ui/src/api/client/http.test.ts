import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { HttpCore, HttpError } from "./http";

class TestClient extends HttpCore {
  request(init?: RequestInit) {
    return this.json("/models", { headers: this.headers(), ...init });
  }
}

beforeEach(() => vi.useFakeTimers());
afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

const client = () =>
  new TestClient("https://tidebreak.example.test", "test-token");
const success = () =>
  new Response(JSON.stringify({ models: [] }), { status: 200 });

describe("JSON read recovery", () => {
  it("recovers a dropped connection and a transient status without losing auth", async () => {
    const fetch = vi
      .fn()
      .mockRejectedValueOnce(new TypeError("Load failed"))
      .mockResolvedValueOnce(new Response("unavailable", { status: 503 }))
      .mockResolvedValueOnce(success());
    vi.stubGlobal("fetch", fetch);
    const result = expect(client().request()).resolves.toEqual({ models: [] });
    await vi.runAllTimersAsync();
    await result;
    expect(fetch).toHaveBeenCalledTimes(3);
    for (const [, init] of fetch.mock.calls) {
      expect(init.headers.Authorization).toBe("Bearer test-token");
    }
  });

  it.each([502, 503, 504])("bounds retries for HTTP %s", async (status) => {
    const fetch = vi
      .fn()
      .mockImplementation(async () => new Response("unavailable", { status }));
    vi.stubGlobal("fetch", fetch);
    const result = expect(client().request()).rejects.toMatchObject({ status });
    await vi.runAllTimersAsync();
    await result;
    expect(fetch).toHaveBeenCalledTimes(3);
  });

  it.each([400, 401, 403, 404, 409, 429, 500])(
    "preserves HTTP %s without retry",
    async (status) => {
      const fetch = vi
        .fn()
        .mockResolvedValue(new Response("refused", { status }));
      vi.stubGlobal("fetch", fetch);
      await expect(client().request()).rejects.toBeInstanceOf(HttpError);
      expect(fetch).toHaveBeenCalledTimes(1);
    },
  );

  it.each(["POST", "PUT", "PATCH", "DELETE"])(
    "never replays a %s after a dropped response",
    async (method) => {
      const fetch = vi.fn().mockRejectedValue(new TypeError("Load failed"));
      vi.stubGlobal("fetch", fetch);
      await expect(client().request({ method })).rejects.toThrow("Load failed");
      expect(fetch).toHaveBeenCalledTimes(1);
    },
  );

  it("never replays a turn after a transient HTTP response", async () => {
    const fetch = vi
      .fn()
      .mockResolvedValue(new Response("unavailable", { status: 503 }));
    vi.stubGlobal("fetch", fetch);
    await expect(client().request({ method: "POST" })).rejects.toMatchObject({
      status: 503,
    });
    expect(fetch).toHaveBeenCalledTimes(1);
  });

  it("stops during backoff when the caller cancels", async () => {
    const fetch = vi.fn().mockRejectedValue(new TypeError("Load failed"));
    vi.stubGlobal("fetch", fetch);
    const controller = new AbortController();
    const result = expect(
      client().request({ signal: controller.signal }),
    ).rejects.toMatchObject({ name: "AbortError" });
    await vi.advanceTimersByTimeAsync(1);
    controller.abort();
    await result;
    await vi.runAllTimersAsync();
    expect(fetch).toHaveBeenCalledTimes(1);
  });

  it("does not send a cancelled request", async () => {
    const fetch = vi.fn();
    vi.stubGlobal("fetch", fetch);
    const controller = new AbortController();
    controller.abort();
    await expect(
      client().request({ signal: controller.signal }),
    ).rejects.toMatchObject({ name: "AbortError" });
    expect(fetch).not.toHaveBeenCalled();
  });

  it("recovers a dropped response body but does not retry malformed JSON", async () => {
    const fetch = vi
      .fn()
      .mockResolvedValueOnce({
        ok: true,
        status: 200,
        text: async () => {
          throw new TypeError("Load failed");
        },
      })
      .mockResolvedValueOnce(success());
    vi.stubGlobal("fetch", fetch);
    const result = expect(client().request()).resolves.toEqual({ models: [] });
    await vi.runAllTimersAsync();
    await result;
    expect(fetch).toHaveBeenCalledTimes(2);
    fetch
      .mockReset()
      .mockResolvedValue(new Response("{invalid", { status: 200 }));
    await expect(client().request()).rejects.toBeInstanceOf(SyntaxError);
    expect(fetch).toHaveBeenCalledTimes(1);
  });
});
