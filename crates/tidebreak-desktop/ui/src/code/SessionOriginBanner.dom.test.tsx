// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { SessionOriginBanner } from "./SessionOriginBanner";

const origin = {
  channel_kind: "slack",
  external_key: "T0400000:C0812345:1724900000.123456",
};

afterEach(cleanup);

describe("SessionOriginBanner", () => {
  it("describes the session's execution location", () => {
    const { rerender } = render(
      <SessionOriginBanner origin={origin} executionLocation="machine" />,
    );
    expect(screen.getByTestId("session-origin-banner")).toHaveTextContent(
      "Started from Slack; runs on this machine.",
    );
    expect(
      screen.getByRole("button", { name: "Open the thread" }),
    ).toBeTruthy();

    rerender(
      <SessionOriginBanner origin={origin} executionLocation="sandbox" />,
    );
    expect(screen.getByTestId("session-origin-banner")).toHaveTextContent(
      "Started from Slack; runs in a sandbox.",
    );
  });

  it("names whether the session acts as you or the bot", () => {
    const { rerender } = render(
      <SessionOriginBanner
        origin={origin}
        executionLocation="machine"
        actsAs="person"
      />,
    );
    expect(screen.getByTestId("session-origin-banner")).toHaveTextContent(
      "Started from Slack; runs on this machine as you.",
    );
    rerender(
      <SessionOriginBanner
        origin={origin}
        executionLocation="machine"
        actsAs="bot"
      />,
    );
    expect(screen.getByTestId("session-origin-banner")).toHaveTextContent(
      "Started from Slack; runs on this machine as the bot.",
    );
  });
  it("links every conversation that reaches the session", () => {
    render(
      <SessionOriginBanner
        origin={origin}
        executionLocation="machine"
        origins={[
          origin,
          {
            channel_kind: "slack",
            external_key: "T0400000:C0865432:1724900010.123456",
          },
        ]}
      />,
    );
    expect(screen.getByText(/Shared across 2 conversations/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "Open thread 1" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Open thread 2" })).toBeTruthy();
  });
});
