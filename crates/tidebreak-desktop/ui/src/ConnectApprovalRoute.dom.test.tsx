// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { CodeConnectPage } from "./api";
import { ConnectApprovalView } from "./ConnectApprovalRoute";

const page: CodeConnectPage = {
  channel_kind: "slack",
  display_name: "Casey Nakamura",
  workspace_name: "Acme Corp",
  avatar_url: "https://avatars.example/casey.png",
  state: "pending",
  csrf: "csrf-token",
  expires_at: "2026-08-29T12:15:00Z",
};

afterEach(cleanup);

describe("ConnectApprovalView", () => {
  it("shows the exact identity, protects the avatar request, and approves once", async () => {
    const onApprove = vi.fn();
    const { container } = render(
      <ConnectApprovalView
        page={page}
        phase="ready"
        error={null}
        onApprove={onApprove}
      />,
    );

    expect(screen.getByText("Casey Nakamura")).toBeTruthy();
    expect(screen.getByText(/Slack · Acme Corp/)).toBeTruthy();
    const avatar = screen.getByRole("img", {
      name: "Casey Nakamura's avatar",
    });
    expect(avatar).toHaveAttribute("referrerPolicy", "no-referrer");
    fireEvent.error(avatar);
    expect(container.querySelector("img")).toBeNull();

    await userEvent
      .setup()
      .click(screen.getByRole("button", { name: "Yes, this is me" }));
    expect(onApprove).toHaveBeenCalledTimes(1);
  });

  it("renders loading, approved, invalid, and failed states without raw errors", () => {
    const { rerender } = render(
      <ConnectApprovalView
        page={null}
        phase="loading"
        error={null}
        onApprove={() => {}}
      />,
    );
    expect(screen.getByRole("status")).toHaveTextContent(
      "Opening the connect request",
    );

    rerender(
      <ConnectApprovalView
        page={{ ...page, state: "approved" }}
        phase="approved"
        error={null}
        onApprove={() => {}}
      />,
    );
    expect(screen.getByText(/return to Slack and confirm there/)).toBeTruthy();
    expect(
      screen.queryByRole("button", { name: "Yes, this is me" }),
    ).toBeNull();

    rerender(
      <ConnectApprovalView
        page={null}
        phase="invalid"
        error={null}
        onApprove={() => {}}
      />,
    );
    expect(
      screen.getByRole("heading", {
        name: "This connect link is no longer valid",
      }),
    ).toBeTruthy();

    rerender(
      <ConnectApprovalView
        page={page}
        phase="ready"
        error="The connect request could not be approved. Try again."
        onApprove={() => {}}
      />,
    );
    expect(screen.getByRole("alert")).toHaveTextContent(
      "The connect request could not be approved. Try again.",
    );
  });
});
