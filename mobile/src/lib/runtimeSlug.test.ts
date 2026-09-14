import { describe, expect, it } from "vitest";
import {
  FALLBACK_RUNTIME_SLUG,
  isMintableSlug,
  runtimeSlugFrom,
} from "./runtimeSlug";
import type { AppListResponse } from "./consoleTypes";

function apps(slugs: (string[] | undefined)[]): AppListResponse {
  return {
    apps: slugs.map((mcp_endpoint_slugs, index) => ({
      id: `app-${index}`,
      name: `App ${index}`,
      app_kind: "rest_api",
      enabled: true,
      ...(mcp_endpoint_slugs ? { mcp_endpoint_slugs } : {}),
    })),
  };
}

describe("runtimeSlugFrom", () => {
  it("names an endpoint the installation really serves when there is one", () => {
    expect(runtimeSlugFrom(apps([[], ["engineering", "ops"]]))).toBe(
      "engineering",
    );
  });

  it("falls back to this client's own name rather than failing", () => {
    // The verbs bind to the user, not to the endpoint, so a member on an
    // installation with no MCP endpoints can still steer their own runs.
    expect(runtimeSlugFrom(apps([[], undefined]))).toBe(FALLBACK_RUNTIME_SLUG);
    expect(runtimeSlugFrom({ apps: [] })).toBe(FALLBACK_RUNTIME_SLUG);
    // A failed catalog read reaches here as null; it must not be fatal.
    expect(runtimeSlugFrom(null)).toBe(FALLBACK_RUNTIME_SLUG);
    expect(runtimeSlugFrom(undefined)).toBe(FALLBACK_RUNTIME_SLUG);
  });

  it("skips a discovered slug the token endpoint would refuse", () => {
    // A slug this client cannot mint is worse than no discovery at all: it
    // would turn a working fallback into a refused mint.
    expect(runtimeSlugFrom(apps([["has space", "ok-one"]]))).toBe("ok-one");
    expect(runtimeSlugFrom(apps([["bad/slug"]]))).toBe(FALLBACK_RUNTIME_SLUG);
  });

  it("keeps a fallback the gateway's own rule accepts", () => {
    expect(isMintableSlug(FALLBACK_RUNTIME_SLUG)).toBe(true);
  });
});

describe("isMintableSlug", () => {
  it("matches the gateway's syntactic rule", () => {
    expect(isMintableSlug("a")).toBe(true);
    expect(isMintableSlug("a_b-C9")).toBe(true);
    expect(isMintableSlug("a".repeat(127))).toBe(true);
    expect(isMintableSlug("")).toBe(false);
    expect(isMintableSlug("a".repeat(128))).toBe(false);
    expect(isMintableSlug("a:b")).toBe(false);
    expect(isMintableSlug("a b")).toBe(false);
    expect(isMintableSlug("a.b")).toBe(false);
  });
});
