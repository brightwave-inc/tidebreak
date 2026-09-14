import { describe, expect, it, vi } from "vitest";
import { GatewayClient, HttpError, parseRefusal } from "./gatewayClient";
import type { HttpFetch } from "./http";

const BASE = "https://gateway.example.test";

function tokens(value = "mg_at_token") {
  return { getAccessToken: vi.fn(async () => value) };
}

function client(fetchImpl: HttpFetch, resource = "control_plane") {
  return new GatewayClient({
    baseUrl: `${BASE}/`,
    resource,
    tokens: tokens(),
    fetchImpl,
  });
}

function json(body: unknown, status = 200): HttpFetch {
  return async () =>
    new Response(JSON.stringify(body), {
      status,
      headers: { "Content-Type": "application/json" },
    }) as unknown as Awaited<ReturnType<HttpFetch>>;
}

describe("parseRefusal", () => {
  // The two shapes are the reason this client exists: a caller that branches
  // on `sandboxes_not_enabled` or `sandbox_already_terminal` must get the same
  // `code` whichever surface refused it.
  it("reads the nested control-plane shape", () => {
    expect(
      parseRefusal(
        { error: { code: "sandboxes_not_enabled", message: "Not enabled." } },
        "fallback",
      ),
    ).toEqual({ code: "sandboxes_not_enabled", message: "Not enabled." });
  });

  it("reads the flat OAuth-shaped runtime refusal", () => {
    expect(
      parseRefusal(
        {
          error: "sandbox_already_terminal",
          error_description: "The sandbox already finished.",
        },
        "fallback",
      ),
    ).toEqual({
      code: "sandbox_already_terminal",
      message: "The sandbox already finished.",
    });
  });

  it("keeps the code when a refusal carries no prose", () => {
    expect(parseRefusal({ error: "invalid_runtime_token" }, "fallback")).toEqual(
      { code: "invalid_runtime_token", message: "fallback" },
    );
    expect(
      parseRefusal({ error: { code: "forbidden" } }, "fallback"),
    ).toEqual({ code: "forbidden", message: "fallback" });
  });

  it("falls back for a body that is neither shape", () => {
    for (const body of [null, "<html>", 7, {}, { error: 3 }]) {
      expect(parseRefusal(body, "fallback")).toEqual({
        code: undefined,
        message: "fallback",
      });
    }
  });
});

describe("GatewayClient", () => {
  it("authenticates with a bearer minted for its own resource", async () => {
    const source = tokens();
    const fetchImpl = vi.fn(json({ data: [] })) as unknown as HttpFetch;
    const gateway = new GatewayClient({
      baseUrl: BASE,
      resource: "runtime:tidebreak-mobile",
      tokens: source,
      fetchImpl,
    });
    await gateway.request("/api/v1/runtime/sandboxes/a");
    expect(source.getAccessToken).toHaveBeenCalledWith(
      "runtime:tidebreak-mobile",
    );
    const call = vi.mocked(fetchImpl).mock.calls[0];
    expect(call?.[0]).toBe(`${BASE}/api/v1/runtime/sandboxes/a`);
    expect(call?.[1]?.headers?.Authorization).toBe("Bearer mg_at_token");
    // Redirects carry the bearer to whatever host the Location names.
    expect(call?.[1]?.redirect).toBe("manual");
  });

  it("trims a trailing slash off the base URL rather than doubling it", async () => {
    const fetchImpl = vi.fn(json({})) as unknown as HttpFetch;
    await client(fetchImpl).request("/api/v1/cli/me");
    expect(vi.mocked(fetchImpl).mock.calls[0]?.[0]).toBe(
      `${BASE}/api/v1/cli/me`,
    );
  });

  it("sends a JSON body and its content type only when there is one", async () => {
    const fetchImpl = vi.fn(json({ seq: 4 })) as unknown as HttpFetch;
    await client(fetchImpl).request("/messages", {
      method: "POST",
      body: { body: "steer", interrupt: false },
    });
    const init = vi.mocked(fetchImpl).mock.calls[0]?.[1];
    expect(init?.method).toBe("POST");
    expect(init?.headers?.["Content-Type"]).toBe("application/json");
    expect(init?.body).toBe(JSON.stringify({ body: "steer", interrupt: false }));
  });

  it("raises the nested refusal as an HttpError carrying its code", async () => {
    const fetchImpl = json(
      { error: { code: "sandboxes_not_enabled", message: "Not enabled." } },
      409,
    );
    await expect(client(fetchImpl).request("/api/v1/admin/sandboxes")).rejects
      .toMatchObject({
        name: "HttpError",
        status: 409,
        code: "sandboxes_not_enabled",
        message: "Not enabled.",
        path: "/api/v1/admin/sandboxes",
      });
  });

  it("raises the flat runtime refusal the same way", async () => {
    const fetchImpl = json(
      { error: "sandbox_unavailable", error_description: "Already finished." },
      403,
    );
    const failure = await client(fetchImpl)
      .request("/cancel", { method: "POST" })
      .catch((error: unknown) => error);
    expect(failure).toBeInstanceOf(HttpError);
    expect((failure as HttpError).code).toBe("sandbox_unavailable");
    expect((failure as HttpError).message).toBe("Already finished.");
  });

  it("reports the status when the refusal body is not JSON", async () => {
    const fetchImpl: HttpFetch = async () =>
      new Response("<html>bad gateway</html>", {
        status: 502,
      }) as unknown as Awaited<ReturnType<HttpFetch>>;
    await expect(client(fetchImpl).request("/x")).rejects.toThrow(/HTTP 502/);
  });

  it("returns nothing for a 204 rather than failing to parse an empty body", async () => {
    const fetchImpl: HttpFetch = async () =>
      new Response(null, { status: 204 }) as unknown as Awaited<
        ReturnType<HttpFetch>
      >;
    await expect(
      client(fetchImpl).request("/cancel", { method: "POST" }),
    ).resolves.toBeUndefined();
  });

  it("refuses a redirect instead of re-sending the bearer to its target", async () => {
    const fetchImpl: HttpFetch = async () =>
      new Response(null, {
        status: 302,
        headers: { Location: "https://evil.example/steal" },
      }) as unknown as Awaited<ReturnType<HttpFetch>>;
    await expect(client(fetchImpl).request("/api/v1/cli/me")).rejects.toThrow(
      /redirect/,
    );
  });
});
