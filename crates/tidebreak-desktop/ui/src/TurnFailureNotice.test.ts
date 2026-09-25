import { describe, expect, it } from "vitest";
import type { TurnFailureCategory } from "./generated/wire";
import {
  turnFailureCopy,
  turnFailureOffersRetry,
  turnFailurePointsAtSettings,
} from "./TurnFailureNotice";

const EVERY_CATEGORY: TurnFailureCategory[] = [
  "rate_limited",
  "overloaded",
  "auth",
  "provider_access",
  "model_unavailable",
  "endpoint_not_found",
  "context_overflow",
  "request_rejected",
  "local",
  "transient",
  "engine_auth",
  "usage_limit",
  "unknown",
];

describe("TurnFailureNotice copy", () => {
  it("attributes a bare provider access denial without inventing one cause", () => {
    const copy = turnFailureCopy("provider_access", "xAI");

    expect(copy.title).toBe("xAI denied access to this request");
    expect(copy.body).toContain("not Tidebreak");
    expect(copy.body).toContain("exhausted credits or quota");
    expect(copy.body).toContain("billing or organization restrictions");
    expect(turnFailurePointsAtSettings("provider_access")).toBe(true);
    expect(turnFailurePointsAtSettings("transient")).toBe(false);
  });

  it("keeps invalid credentials separate from provider account access", () => {
    const copy = turnFailureCopy("auth", "xAI");

    expect(copy.title).toBe("Tidebreak has no working credential for xAI");
    expect(copy.body).toContain("API key");
    expect(copy.body).not.toContain("credits");
  });

  /**
   * The same category covers a key that was never saved, which no provider
   * saw. The copy must not say the provider refused it.
   */
  it("does not blame the provider for a credential that may be missing", () => {
    const copy = turnFailureCopy("auth", "OpenAI");

    expect(copy.title).not.toContain("could not authenticate");
    expect(copy.body).toContain("missing");

    expect(turnFailureCopy("auth").title).toBe(
      "Tidebreak has no working credential for the model provider",
    );
  });

  it("writes copy for every category the wire can carry", () => {
    for (const category of EVERY_CATEGORY) {
      const copy = turnFailureCopy(category, "Anthropic", "claude-opus-5");
      expect(copy.title.length, category).toBeGreaterThan(0);
      expect(copy.body.length, category).toBeGreaterThan(0);
    }
  });

  /**
   * A newer server can send a category this build has no copy for. Reading
   * it as unknown keeps the transcript drawable instead of throwing.
   */
  it("reads a category from a newer server as unknown", () => {
    const newer = turnFailureCopy(
      "a_category_from_next_year" as TurnFailureCategory,
    );
    expect(newer).toEqual(turnFailureCopy("unknown"));
  });

  it("names the retired model and does not blame the connection", () => {
    const copy = turnFailureCopy(
      "model_unavailable",
      "Anthropic",
      "claude-3-opus-20240229",
    );
    expect(copy.title).toBe(
      "claude-3-opus-20240229 is not available from Anthropic",
    );
    expect(copy.body).toContain("Choose another model");
    expect(copy.body).not.toContain("connection");
  });

  it("keeps overload apart from the reader's own quota", () => {
    expect(turnFailureCopy("overloaded", "Anthropic").body).not.toContain(
      "quota",
    );
    expect(turnFailureCopy("rate_limited", "Anthropic").body).toContain(
      "quota",
    );
  });

  /**
   * The server may run on a hosted machine, so the copy blames neither the
   * provider nor "this computer" and its keychain.
   */
  it("does not blame the provider or this computer for a local fault", () => {
    const copy = turnFailureCopy("local", "Anthropic");
    expect(copy.title).not.toContain("Anthropic");
    expect(copy.body).toContain("not from the provider");
    expect(copy.body).not.toContain("this computer");
    expect(copy.body).not.toContain("keychain");
  });

  it("points a 404 without a model at the provider's address", () => {
    const copy = turnFailureCopy("endpoint_not_found", "OpenRouter");
    expect(copy.title).toContain("OpenRouter");
    expect(copy.body).toContain("base URL");
    expect(turnFailurePointsAtSettings("endpoint_not_found")).toBe(true);
  });

  /**
   * Retry is off for an oversized conversation until a model switch can
   * rerun the turn in place, so its copy suggests nothing that needs one.
   */
  it("does not suggest a bigger model while no retry is offered", () => {
    expect(turnFailureOffersRetry("context_overflow")).toBe(false);
    expect(turnFailureCopy("context_overflow").body).not.toContain("model");
  });

  /**
   * Running the same request again cannot fit an oversized conversation or
   * change a refusal, so those two offer no Retry. The rest keep it for
   * after their fix.
   */
  it("offers Retry only where running the turn again can help", () => {
    expect(turnFailureOffersRetry("context_overflow")).toBe(false);
    expect(turnFailureOffersRetry("request_rejected")).toBe(false);
    for (const category of [
      "rate_limited",
      "overloaded",
      "transient",
      "auth",
      "model_unavailable",
      "local",
      "unknown",
    ] as const) {
      expect(turnFailureOffersRetry(category), category).toBe(true);
    }
  });
});
