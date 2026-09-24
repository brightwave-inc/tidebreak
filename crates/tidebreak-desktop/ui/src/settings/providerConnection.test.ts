import { describe, expect, it } from "vitest";

import type { ProviderTestResult } from "../api";
import { providerInfoFixture } from "../stories/fixtures";
import {
  loopbackIpHttpHost,
  providerBadge,
  providerTestable,
  testedAgo,
} from "./providerConnection";

const now = new Date("2026-09-23T12:00:00Z");

function tested(
  outcome: ProviderTestResult["outcome"],
  secondsAgo = 5,
): ProviderTestResult {
  return {
    outcome,
    message: "Stand-in message.",
    tested_at: new Date(now.getTime() - secondsAgo * 1000).toISOString(),
  };
}

describe("provider badge", () => {
  const anthropic = providerInfoFixture("anthropic", {
    enabled: true,
    has_credential: true,
  });

  it("says what the last test found, not that a key is stored", () => {
    expect(providerBadge(anthropic, false, null)).toEqual({
      label: "Not tested",
      variant: "outline",
    });
    expect(providerBadge(anthropic, false, tested("connected"))).toEqual({
      label: "Connected",
      variant: "success",
    });
    expect(providerBadge(anthropic, false, tested("key_rejected"))).toEqual({
      label: "Key rejected",
      variant: "critical",
    });
    expect(providerBadge(anthropic, false, tested("rate_limited"))).toEqual({
      label: "Rate limited",
      variant: "warning",
    });
    expect(
      providerBadge(anthropic, false, tested("unexpected_answer")).label,
    ).toBe("Unexpected answer");
    expect(providerBadge(anthropic, true, tested("connected")).label).toBe(
      "Testing…",
    );
  });

  it("does not vouch for a provider that cannot run", () => {
    const off = { ...anthropic, enabled: false };
    expect(providerBadge(off, false, tested("connected")).label).toBe(
      "Not connected",
    );
    const keyless = providerInfoFixture("gemini", { enabled: true });
    expect(providerBadge(keyless, false, null).label).toBe("Not connected");
  });

  it("reads ChatGPT sign-in as signed in, with nothing to test", () => {
    const chatgpt = providerInfoFixture("openai", {
      enabled: true,
      has_credential: true,
      auth_mode: "chatgpt",
    });
    expect(providerBadge(chatgpt, false, null).label).toBe("Signed in");
    expect(providerTestable(chatgpt)).toBe(false);
  });

  it("tests local servers without a key, once they have an address", () => {
    expect(
      providerTestable(providerInfoFixture("ollama", { enabled: true })),
    ).toBe(true);
    expect(
      providerTestable(
        providerInfoFixture("openai_compatible", { enabled: true }),
      ),
    ).toBe(false);
    expect(
      providerTestable(
        providerInfoFixture("openai_compatible", {
          enabled: true,
          base_url: "http://127.0.0.1:1234/v1",
        }),
      ),
    ).toBe(true);
  });
});

describe("tested time", () => {
  it("reads as a person would say it", () => {
    expect(testedAgo(tested("connected", 5).tested_at, now)).toBe("just now");
    expect(testedAgo("not a time", now)).toBe("at an unknown time");
  });
});

describe("clear-text loopback addresses", () => {
  it("names a loopback IP literal and nothing else", () => {
    expect(loopbackIpHttpHost("http://127.0.0.1:1234/v1")).toBe("127.0.0.1");
    expect(loopbackIpHttpHost(" http://127.9.9.9:8080 ")).toBe("127.9.9.9");
    expect(loopbackIpHttpHost("http://[::1]:1234/v1")).toBe("[::1]");
    expect(loopbackIpHttpHost("http://localhost:1234/v1")).toBeNull();
    expect(loopbackIpHttpHost("https://127.0.0.1:1234/v1")).toBeNull();
    expect(loopbackIpHttpHost("http://192.168.1.10:1234/v1")).toBeNull();
    expect(loopbackIpHttpHost("http://user@127.0.0.1:1234/v1")).toBeNull();
    expect(loopbackIpHttpHost("not a url")).toBeNull();
  });
});
