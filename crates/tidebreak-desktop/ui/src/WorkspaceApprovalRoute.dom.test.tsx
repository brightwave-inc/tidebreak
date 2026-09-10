// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { CodeConnectPage } from "./api";
import { WorkspaceApprovalView } from "./WorkspaceApprovalRoute";

const page: CodeConnectPage = {
  channel_kind: "slack",
  display_name: "tidebreak-slack",
  workspace_name: "Acme Corp",
  state: "pending",
  csrf: "csrf-token",
  expires_at: "2026-09-08T12:15:00Z",
};

afterEach(cleanup);

describe("WorkspaceApprovalView", () => {
  it("asks an admin to run channel sessions as the shared identity", async () => {
    const onApprove = vi.fn();
    render(
      <WorkspaceApprovalView
        page={page}
        phase="ready"
        error={null}
        onApprove={onApprove}
        onRetry={() => {}}
      />,
    );

    expect(screen.getByText("Acme Corp")).toBeTruthy();
    expect(
      screen.getByText(
        /Run channel sessions for Slack workspace Acme Corp as tidebreak-slack/,
      ),
    ).toBeTruthy();

    expect(
      screen.getByText(
        /Every channel can use the repositories available to this instance’s GitHub App/,
      ),
    ).toBeTruthy();
    expect(screen.queryByText(/Settings → Channels/)).toBeNull();

    await userEvent
      .setup()
      .click(screen.getByRole("button", { name: "Approve workspace" }));
    expect(onApprove).toHaveBeenCalledTimes(1);
  });

  it("renders loading, approved, and invalid states", () => {
    const { rerender } = render(
      <WorkspaceApprovalView
        page={null}
        phase="loading"
        error={null}
        onApprove={() => {}}
        onRetry={() => {}}
      />,
    );
    expect(screen.getByRole("status")).toHaveTextContent(
      "Opening the workspace grant",
    );

    rerender(
      <WorkspaceApprovalView
        page={{ ...page, state: "approved" }}
        phase="approved"
        error={null}
        onApprove={() => {}}
        onRetry={() => {}}
      />,
    );
    expect(screen.getByText(/run as tidebreak-slack/)).toBeTruthy();
  });
});
