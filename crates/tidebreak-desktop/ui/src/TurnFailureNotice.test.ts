import { describe, expect, it } from "vitest";
import {
  turnFailureCopy,
  turnFailureOffersRetry,
} from "./TurnFailureNotice";

describe("TurnFailureNotice copy", () => {
  it("attributes a bare provider access denial without inventing one cause", () => {
    const copy = turnFailureCopy("provider_access", "xAI");

    expect(copy.title).toBe("xAI denied access to this request");
    expect(copy.body).toContain("not Tidebreak");
    expect(copy.body).toContain("exhausted credits or quota");
    expect(copy.body).toContain("billing or organization restrictions");
    expect(turnFailureOffersRetry("provider_access")).toBe(false);
  });

  it("keeps invalid credentials separate from provider account access", () => {
    const copy = turnFailureCopy("auth", "xAI");

    expect(copy.title).toContain("authenticate");
    expect(copy.body).toContain("API key");
    expect(copy.body).not.toContain("credits");
  });
});
