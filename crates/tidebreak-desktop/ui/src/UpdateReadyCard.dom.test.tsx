// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { releaseNotesUrl, UpdateReadyCard } from "./UpdateReadyCard";

const openInBrowser = vi.hoisted(() => vi.fn(async () => {}));
vi.mock("./openInBrowser", () => ({ openInBrowser }));

afterEach(cleanup);

describe("UpdateReadyCard", () => {
  it("shows an indeterminate check without update actions", () => {
    const { container } = render(
      <UpdateReadyCard status="checking" version={null} onDismiss={vi.fn()} />,
    );

    expect(screen.getByLabelText("Checking for updates")).toHaveTextContent(
      "Looking for a newer version of Tidebreak",
    );
    expect(container.querySelector(".animate-spin")).not.toBeNull();
    expect(
      screen.queryByRole("button", { name: "Restart and update" }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Dismiss update notice" }),
    ).toBeInTheDocument();
  });

  it("offers the update, release notes, and dismissal", async () => {
    const user = userEvent.setup();
    const onRestart = vi.fn();
    const onDismiss = vi.fn();
    render(
      <UpdateReadyCard
        version="0.59.0"
        onRestart={onRestart}
        onDismiss={onDismiss}
      />,
    );

    expect(
      screen.getByText("Tidebreak 0.59.0 is downloaded and ready to install."),
    ).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Release notes" }));
    expect(openInBrowser).toHaveBeenCalledWith(
      "https://github.com/brightwave-inc/tidebreak/releases/tag/v0.59.0",
    );

    await user.click(
      screen.getByRole("button", { name: "Restart and update" }),
    );
    expect(onRestart).toHaveBeenCalledOnce();

    await user.click(
      screen.getByRole("button", { name: "Dismiss update notice" }),
    );
    expect(onDismiss).toHaveBeenCalledOnce();
  });

  it("links to the latest release when the version is unavailable", () => {
    expect(releaseNotesUrl(null)).toBe(
      "https://github.com/brightwave-inc/tidebreak/releases/latest",
    );
  });
});
