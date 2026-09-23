// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  releaseNotesUrl,
  splitUpdateMessage,
  UpdateReadyCard,
} from "./UpdateReadyCard";

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

  it("offers the download for an update that waits to be downloaded", async () => {
    const user = userEvent.setup();
    const onDownload = vi.fn();
    render(
      <UpdateReadyCard
        status="available"
        version="0.115.0"
        onDownload={onDownload}
        onDismiss={vi.fn()}
      />,
    );

    expect(screen.getByLabelText("Update available")).toHaveTextContent(
      "Tidebreak 0.115.0 is available.",
    );
    expect(
      screen.queryByRole("button", { name: "Restart and update" }),
    ).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Download" }));
    expect(onDownload).toHaveBeenCalledOnce();
  });

  it("reports a check that found nothing newer, with your version", () => {
    render(
      <UpdateReadyCard
        status="up-to-date"
        version="0.114.0"
        onDismiss={vi.fn()}
      />,
    );

    expect(screen.getByLabelText("You're up to date")).toHaveTextContent(
      "Tidebreak 0.114.0 is the latest version.",
    );
    expect(screen.queryByRole("button", { name: "Download" })).toBeNull();
  });

  it("reports why a check failed", () => {
    render(
      <UpdateReadyCard
        status="failed"
        message="Could not check for updates. Tidebreak could not reach the update server. Check your internet connection and try again."
        onDismiss={vi.fn()}
      />,
    );

    expect(
      screen.getByLabelText("Could not check for updates"),
    ).toHaveTextContent(
      "Tidebreak could not reach the update server. Check your internet connection and try again.",
    );
  });

  it("splits an update error into what failed and why", () => {
    expect(
      splitUpdateMessage("Could not prepare the update. Try again later."),
    ).toEqual({
      title: "Could not prepare the update",
      detail: "Try again later.",
    });
    expect(splitUpdateMessage("Update controls are unavailable.")).toEqual({
      title: "Update controls are unavailable",
      detail: null,
    });
  });
});
