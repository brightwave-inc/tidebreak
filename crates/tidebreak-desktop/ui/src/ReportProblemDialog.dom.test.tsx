// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ReportProblemDialog } from "./ReportProblemDialog";
import type { ProblemReportFacts } from "./reportProblem";

const FACTS: ProblemReportFacts = {
  version: "0.117.0",
  os: "macOS 15.6",
  arch: "arm64",
};

const ISSUE =
  "https://github.com/brightwave-inc/tidebreak/issues/new?template=bug-report.yml&version=0.117.0&os=macOS+15.6&arch=arm64";

afterEach(cleanup);

function renderDialog(
  over: Partial<Parameters<typeof ReportProblemDialog>[0]> = {},
) {
  const props = {
    open: true,
    onOpenChange: vi.fn(),
    readFacts: vi.fn(async () => FACTS),
    saveReport: vi.fn(async () => true),
    openUrl: vi.fn(async (_url: string) => {}),
    ...over,
  };
  render(<ReportProblemDialog {...props} />);
  return props;
}

describe("ReportProblemDialog", () => {
  it("shows exactly what the issue will carry before anything opens", async () => {
    const { openUrl, saveReport } = renderDialog();
    expect(
      await screen.findByText("Tidebreak 0.117.0 · macOS 15.6 · arm64"),
    ).toBeVisible();
    expect(openUrl).not.toHaveBeenCalled();
    expect(saveReport).not.toHaveBeenCalled();
  });

  it("saves the report, then opens the prefilled issue", async () => {
    const user = userEvent.setup();
    const { openUrl, saveReport } = renderDialog();
    await screen.findByText("Tidebreak 0.117.0 · macOS 15.6 · arm64");

    await user.click(
      screen.getByRole("button", { name: "Save report and open issue" }),
    );

    expect(saveReport).toHaveBeenCalledOnce();
    await waitFor(() => expect(openUrl).toHaveBeenCalledWith(ISSUE));
    expect(await screen.findByText("Report saved")).toBeVisible();
  });

  it("opens nothing when the save dialog is closed", async () => {
    const user = userEvent.setup();
    const { openUrl } = renderDialog({ saveReport: vi.fn(async () => false) });
    await screen.findByText("Tidebreak 0.117.0 · macOS 15.6 · arm64");

    await user.click(
      screen.getByRole("button", { name: "Save report and open issue" }),
    );

    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Save report and open issue" }),
      ).toBeEnabled(),
    );
    expect(openUrl).not.toHaveBeenCalled();
  });

  it("says a save failed and still offers the issue", async () => {
    const user = userEvent.setup();
    const { openUrl } = renderDialog({
      saveReport: vi.fn(async () => {
        throw "Could not build the diagnostics report";
      }),
    });
    await screen.findByText("Tidebreak 0.117.0 · macOS 15.6 · arm64");

    await user.click(
      screen.getByRole("button", { name: "Save report and open issue" }),
    );

    const failure = await screen.findByRole("alert");
    expect(failure).toHaveTextContent("Could not build the diagnostics report");
    expect(failure).toHaveTextContent(
      "You can still open the issue without it.",
    );
    await user.click(
      screen.getByRole("button", { name: "Open issue without a report" }),
    );
    expect(openUrl).toHaveBeenCalledWith(ISSUE);
  });

  it("hands a workspace's report to an agent when asked", async () => {
    const user = userEvent.setup();
    const onAskAgent = vi.fn();
    const { onOpenChange, saveReport } = renderDialog({ onAskAgent });

    await user.click(screen.getByRole("button", { name: /Ask an agent/ }));

    expect(onAskAgent).toHaveBeenCalledOnce();
    expect(onOpenChange).toHaveBeenCalledWith(false);
    expect(saveReport).not.toHaveBeenCalled();
  });

  it("offers the agent only where a session can go with it", async () => {
    renderDialog();
    await screen.findByText("Tidebreak 0.117.0 · macOS 15.6 · arm64");
    expect(
      screen.queryByRole("button", { name: /Ask an agent/ }),
    ).not.toBeInTheDocument();
  });
});
