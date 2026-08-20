import { describe, expect, it } from "vitest";

import {
  browserDisplayAddress,
  browserSecurity,
  browserTarget,
  MAX_BROWSER_URL_CHARS,
  validateBrowserUrl,
} from "./browserNavigation";

describe("browser navigation", () => {
  it("normalizes local servers, private hosts, and public sites", () => {
    expect(browserTarget("localhost:3000")).toEqual({
      ok: true,
      url: "http://localhost:3000/",
    });
    expect(browserTarget("192.168.1.4:5173/app")).toEqual({
      ok: true,
      url: "http://192.168.1.4:5173/app",
    });
    expect(browserTarget("[::1]:4173/app")).toEqual({
      ok: true,
      url: "http://[::1]:4173/app",
    });
    expect(browserTarget("docs.rs/tauri")).toEqual({
      ok: true,
      url: "https://docs.rs/tauri",
    });
  });

  it("searches plain language and rejects unsafe schemes and credentials", () => {
    expect(browserTarget("tauri child webview")).toEqual({
      ok: true,
      url: "https://www.google.com/search?q=tauri%20child%20webview",
    });
    expect(browserTarget("file:///tmp/token")).toMatchObject({ ok: false });
    expect(browserTarget("javascript:alert(1)")).toMatchObject({ ok: false });
    expect(validateBrowserUrl("https://user:secret@example.com")).toMatchObject({
      ok: false,
    });
    expect(
      validateBrowserUrl(
        `https://example.com/${"a".repeat(MAX_BROWSER_URL_CHARS)}`,
      ),
    ).toEqual({ ok: false, message: "That address is too long" });
  });

  it("derives honest security labels from the committed URL", () => {
    expect(browserSecurity("https://example.com")).toEqual({
      kind: "secure",
      label: "Secure",
    });
    expect(browserSecurity("http://localhost:3000")).toEqual({
      kind: "local",
      label: "Local",
    });
    expect(browserSecurity("http://[::1]:3000")).toEqual({
      kind: "local",
      label: "Local",
    });
    expect(browserSecurity("http://example.com")).toEqual({
      kind: "insecure",
      label: "Not secure",
    });
    expect(browserDisplayAddress("https://example.com/docs?q=one")).toBe(
      "example.com/docs?q=one",
    );
    expect(browserDisplayAddress("http://example.com/docs")).toBe(
      "http://example.com/docs",
    );
  });
});
