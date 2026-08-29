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
  });

  it("yields no link for a DM generation key", () => {
    // A DM key carries a generation, not a thread timestamp; guessing a
    // permalink from it would open the wrong place.
    expect(
      externalThreadUrl({
        channel_kind: "slack",
        external_key: "T0400000:D0898765:dm:2",
      }),
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
