import { describe, expect, it } from "vitest";
import {
  REASON_REQUIRES_TLS,
  REASON_URL_INVALID,
  UrlValidationError,
  validatedBaseUrl,
} from "./url";

function reason(raw: string): string {
  try {
    validatedBaseUrl(raw);
    throw new Error("expected refusal");
  } catch (error) {
    if (error instanceof UrlValidationError) {
      return error.reason;
    }
    throw error;
  }
}

describe("validatedBaseUrl", () => {
  it("accepts https and strips trailing slash and whitespace", () => {
    expect(validatedBaseUrl("  https://machine.example.com/  ")).toBe(
      "https://machine.example.com",
    );
    expect(validatedBaseUrl("https://proxy.example.com/tidebreak/")).toBe(
      "https://proxy.example.com/tidebreak",
    );
  });

  it("refuses cleartext off loopback", () => {
    expect(reason("http://machine.example.com")).toBe(REASON_REQUIRES_TLS);
    expect(reason("http://10.0.0.4:8080")).toBe(REASON_REQUIRES_TLS);
    expect(reason("http://localhost.example.com")).toBe(REASON_REQUIRES_TLS);
  });

  it("allows cleartext on loopback", () => {
    expect(validatedBaseUrl("http://localhost:8080")).toBe(
      "http://localhost:8080",
    );
    expect(validatedBaseUrl("http://127.0.0.1:8080")).toBe(
      "http://127.0.0.1:8080",
    );
    expect(validatedBaseUrl("http://[::1]:8080")).toBe("http://[::1]:8080");
  });

  it("refuses unusable addresses before any probe", () => {
    expect(reason("")).toBe(REASON_URL_INVALID);
    expect(reason("machine.example.com")).toBe(REASON_URL_INVALID);
    expect(reason("ftp://machine.example.com")).toBe(REASON_URL_INVALID);
    expect(reason("https://user:pw@machine.example.com")).toBe(
      REASON_URL_INVALID,
    );
    expect(reason("https://machine.example.com?token=abc")).toBe(
      REASON_URL_INVALID,
    );
    expect(reason("https://machine.example.com#fragment")).toBe(
      REASON_URL_INVALID,
    );
  });
});
