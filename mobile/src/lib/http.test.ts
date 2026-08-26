import { describe, expect, it, vi } from "vitest";
import { fetchRefusingRedirects } from "./http";

const TARGET = "https://machine.example.com/policy";

describe("fetchRefusingRedirects", () => {
  it("asks the runtime not to follow and refuses a 3xx answer", async () => {
    const fetchImpl = vi.fn(
      async (_url: RequestInfo | URL, _init?: RequestInit) =>
        new Response(null, {
          status: 307,
          headers: { Location: "https://evil.example/steal" },
        }),
    );
    await expect(
      fetchRefusingRedirects(fetchImpl as unknown as typeof fetch, TARGET),
    ).rejects.toThrow(/redirect/);
    expect(fetchImpl).toHaveBeenCalledTimes(1);
    expect(fetchImpl.mock.calls[0]?.[1]).toMatchObject({ redirect: "manual" });
  });

  it("refuses a response the runtime redirected on its own", async () => {
    const followed = {
      ok: true,
      status: 200,
      type: "basic",
      redirected: true,
      url: "https://evil.example/steal",
    } as unknown as Response;
    await expect(
      fetchRefusingRedirects(async () => followed, TARGET),
    ).rejects.toThrow(/redirect/);
  });

  it("passes a clean response through", async () => {
    const response = await fetchRefusingRedirects(
      async () => new Response("{}", { status: 200 }),
      TARGET,
    );
    expect(response.status).toBe(200);
  });
});
