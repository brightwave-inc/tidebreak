import { describe, expect, it, vi } from "vitest";
import { fetchRefusingRedirects } from "./http";

// The default transport must be expo/fetch: React Native's global fetch
// follows redirects at the native layer before JS can refuse them.
vi.mock("expo/fetch", () => ({
  fetch: vi.fn(
    async (_url: string, _init?: RequestInit) =>
      new Response("{}", { status: 200 }),
  ),
}));

const TARGET = "https://machine.example.com/policy";

describe("fetchRefusingRedirects", () => {
  it("uses expo/fetch with redirects disabled when no transport is given", async () => {
    const { fetch: expoFetch } = await import("expo/fetch");
    const response = await fetchRefusingRedirects(TARGET);
    expect(response.status).toBe(200);
    expect(expoFetch).toHaveBeenCalledTimes(1);
    const mocked = vi.mocked(expoFetch);
    expect(mocked.mock.calls[0]?.[0]).toBe(TARGET);
    expect(mocked.mock.calls[0]?.[1]).toMatchObject({ redirect: "manual" });
  });

  it("asks the transport not to follow and refuses a 3xx answer", async () => {
    const fetchImpl = vi.fn(
      async (_url: string, _init?: RequestInit) =>
        new Response(null, {
          status: 307,
          headers: { Location: "https://evil.example/steal" },
        }),
    );
    await expect(
      fetchRefusingRedirects(TARGET, undefined, fetchImpl),
    ).rejects.toThrow(/redirect/);
    expect(fetchImpl).toHaveBeenCalledTimes(1);
    expect(fetchImpl.mock.calls[0]?.[1]).toMatchObject({ redirect: "manual" });
  });

  it("refuses a response the transport redirected on its own", async () => {
    const followed = {
      ok: true,
      status: 200,
      type: "basic",
      redirected: true,
      url: "https://evil.example/steal",
    } as unknown as Response;
    await expect(
      fetchRefusingRedirects(TARGET, undefined, async () => followed),
    ).rejects.toThrow(/redirect/);
  });

  it("passes a clean response through", async () => {
    const response = await fetchRefusingRedirects(
      TARGET,
      undefined,
      async () => new Response("{}", { status: 200 }),
    );
    expect(response.status).toBe(200);
  });
});
