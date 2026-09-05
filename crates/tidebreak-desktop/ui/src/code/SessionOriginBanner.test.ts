import { describe, expect, it } from "vitest";

import { externalThreadUrl } from "./SessionOriginBanner";

describe("externalThreadUrl", () => {
  it("derives the Slack permalink from a thread key", () => {
    expect(
      externalThreadUrl({
        channel_kind: "slack",
        external_key: "T0400000:C0812345:1724900000.123456",
      }),
    ).toBe("https://slack.com/archives/C0812345/p1724900000123456");
    expect(
      externalThreadUrl({
        channel_kind: "slack",
        external_key: "T1/C1/171.5",
      }),
    ).toBe("https://slack.com/archives/C1/p1715");
  });

  it("yields no link for a DM generation key", () => {
    // A DM key carries a generation, not a thread timestamp; guessing a
    // permalink from it would open the wrong place.
    expect(
      externalThreadUrl({
        channel_kind: "slack",
        external_key: "T0400000:D0898765:dm2",
      }),
    ).toBeNull();
  });

  it.each([
    "T1:C1:not-a-timestamp",
    "T1:C1:171.5:extra",
    "T1:C1/redirect:171.5",
    "T1:C1:171.5?redirect=elsewhere",
  ])("rejects malformed thread key %s", (external_key) => {
    expect(
      externalThreadUrl({ channel_kind: "slack", external_key }),
    ).toBeNull();
  });

  it("yields no link for another channel family", () => {
    expect(
      externalThreadUrl({
        channel_kind: "matrix",
        external_key: "!room:example.org",
      }),
    ).toBeNull();
  });
});
