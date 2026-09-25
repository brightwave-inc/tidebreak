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

    expect(screen.getByLabelText("Tidebreak is up to date")).toHaveTextContent(
      "You're on the latest version, 0.114.0.",
    );
    expect(screen.queryByRole("button", { name: "Download" })).toBeNull();
  });

  it("says why a download failed and offers it again", () => {
    render(
      <UpdateReadyCard
        status="available"
        version="0.115.0"
        error="Not enough disk space to download the update. Free up space, then try again."
        onDownload={vi.fn()}
        onDismiss={vi.fn()}
      />,
    );

    const failure = screen.getByRole("alert");
    expect(failure).toHaveTextContent(
      "Not enough disk space to download the update",
    );
    expect(failure).toHaveTextContent("Free up space, then try again.");
    expect(screen.getByRole("button", { name: "Download" })).toBeEnabled();
  });

  it("shows a refused restart with the action it needs, and keeps the restart", async () => {
    const user = userEvent.setup();
    const onRestart = vi.fn();
    render(
      <UpdateReadyCard
        version="0.117.0"
        error="A code session is still working on a turn. Stop the running turn, or let it finish, then restart. The update stays ready."
        onRestart={onRestart}
        onDismiss={vi.fn()}
      />,
    );

    const refusal = screen.getByRole("status");
    expect(refusal).toHaveTextContent(
      "A code session is still working on a turn",
    );
    expect(refusal).toHaveTextContent(
      "Stop the running turn, or let it finish, then restart.",
    );
    // A refusal is not a failure: the update is still ready to install.
    expect(refusal).toHaveAttribute("data-tone", "warning");
    await user.click(
      screen.getByRole("button", { name: "Restart and update" }),
    );
    expect(onRestart).toHaveBeenCalledOnce();
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

  it("runs a failed check again from the card", async () => {
    const user = userEvent.setup();
    const onRetry = vi.fn();
    render(
      <UpdateReadyCard
        status="failed"
        message="Could not check for updates. Try again later."
        onRetry={onRetry}
        onDismiss={vi.fn()}
      />,
    );
    await user.click(screen.getByRole("button", { name: /Try again/ }));
    expect(onRetry).toHaveBeenCalledOnce();
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
