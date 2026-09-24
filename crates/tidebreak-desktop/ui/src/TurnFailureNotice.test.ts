import { describe, expect, it } from "vitest";
import {
  turnFailureCopy,
  turnFailurePointsAtSettings,
} from "./TurnFailureNotice";

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
});
