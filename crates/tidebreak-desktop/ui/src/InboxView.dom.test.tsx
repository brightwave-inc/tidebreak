// @vitest-environment jsdom
import { cleanup, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { InboxView } from "./InboxView";
import { useInbox } from "./Inbox";
import { renderWithRouter } from "./test/router";

afterEach(() => {
  cleanup();
  useInbox.getState().clear();
});

describe("InboxView", () => {
  it("shows skeleton rows while the inbox has not loaded", async () => {
    useInbox.setState({ entries: [], loaded: false });
    await renderWithRouter(<InboxView />, { initialUrl: "/inbox" });

    expect(screen.getByRole("status", { name: "Loading inbox" })).toBeVisible();
    expect(screen.queryByText("Nothing is waiting")).not.toBeInTheDocument();
    expect(screen.queryByText("Loading…")).not.toBeInTheDocument();
  });
});
