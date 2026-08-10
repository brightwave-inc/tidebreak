// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, expect, it } from "vitest";

import { useConfirm } from "./ConfirmDialog";

afterEach(cleanup);

function ConfirmationQueueHarness() {
  const { confirm, dialog } = useConfirm();
  const [results, setResults] = useState<boolean[] | null>(null);

  return (
    <>
      <button
        type="button"
        onClick={() => {
          const first = confirm({
            title: "First confirmation",
            confirmLabel: "Delete first",
            destructive: true,
          });
          const second = confirm({
            title: "Second confirmation",
            confirmLabel: "Delete second",
            destructive: true,
          });
          void Promise.all([first, second]).then(setResults);
        }}
      >
        Queue confirmations
      </button>
      {results && <output>{JSON.stringify(results)}</output>}
      {dialog}
    </>
  );
}

it("closes between queued confirmations and puts focus on the next cancel", async () => {
  const user = userEvent.setup();
  render(<ConfirmationQueueHarness />);

  await user.click(screen.getByRole("button", { name: "Queue confirmations" }));
  expect(
    await screen.findByRole("heading", { name: "First confirmation" }),
  ).toBeInTheDocument();
  expect(
    screen.queryByRole("heading", { name: "Second confirmation" }),
  ).not.toBeInTheDocument();

  await user.click(screen.getByRole("button", { name: "Delete first" }));
  expect(
    await screen.findByRole("heading", { name: "Second confirmation" }),
  ).toBeInTheDocument();

  const cancel = screen.getByRole("button", { name: "Cancel" });
  await waitFor(() => expect(cancel).toHaveFocus());
  await user.keyboard("[Space]");
  await waitFor(() => expect(screen.getByText("[true,false]")).toBeVisible());
});
