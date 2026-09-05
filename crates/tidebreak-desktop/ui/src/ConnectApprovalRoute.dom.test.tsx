// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { HttpError, type CodeConnectPage } from "./api";
import {
  connectPageFailurePhase,
  ConnectApprovalView,
} from "./ConnectApprovalRoute";

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
        onRetry={() => {}}
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
        onRetry={() => {}}
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
        onRetry={() => {}}
      />,
    );
    expect(
      screen.getByText(/return to the Slack conversation where you started/),
    ).toBeTruthy();
    expect(
      screen.queryByRole("button", { name: "Yes, this is me" }),
    ).toBeNull();

    rerender(
      <ConnectApprovalView
        page={null}
        phase="invalid"
        error={null}
        onApprove={() => {}}
        onRetry={() => {}}
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
        onRetry={() => {}}
      />,
    );
    expect(screen.getByRole("alert")).toHaveTextContent(
      "The connect request could not be approved. Try again.",
    );
  });

  it("keeps temporary load failures retryable", async () => {
    const onRetry = vi.fn();
    render(
      <ConnectApprovalView
        page={null}
        phase="unavailable"
        error={null}
        onApprove={() => {}}
        onRetry={onRetry}
      />,
    );

    expect(screen.getByRole("alert")).toHaveTextContent(
      "The link may still be valid",
    );
    await userEvent
      .setup()
      .click(screen.getByRole("button", { name: "Try again" }));
    expect(onRetry).toHaveBeenCalledOnce();
  });

  it("calls only a used-or-stale 404 invalid", () => {
    expect(connectPageFailurePhase(new HttpError(404, "not found"))).toBe(
      "invalid",
    );
    expect(connectPageFailurePhase(new HttpError(401, "unauthorized"))).toBe(
      "unavailable",
    );
    expect(connectPageFailurePhase(new HttpError(500, "server error"))).toBe(
      "unavailable",
    );
    expect(connectPageFailurePhase(new TypeError("network error"))).toBe(
      "unavailable",
    );
  });
});
