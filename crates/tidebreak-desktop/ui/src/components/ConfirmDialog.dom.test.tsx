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

function DecisionHarness() {
  const { decide, dialog } = useConfirm();
  const [result, setResult] = useState<string | null>(null);
  return (
    <>
      <button
        type="button"
        onClick={() =>
          void decide({
            title: "Delete the brief?",
            alternativeLabel: "Archive",
            confirmLabel: "Delete work",
            destructive: true,
          }).then(setResult)
        }
      >
        Ask
      </button>
      {result && <output>{result}</output>}
      {dialog}
    </>
  );
}

it.each([
  ["Archive", "alternative"],
  ["Delete work", "confirm"],
  ["Cancel", "cancel"],
])("answers %s with %s", async (button, expected) => {
  const user = userEvent.setup();
  render(<DecisionHarness />);
  await user.click(screen.getByRole("button", { name: "Ask" }));
  await user.click(await screen.findByRole("button", { name: button }));
  await waitFor(() => expect(screen.getByText(expected)).toBeVisible());
});

function TypedConfirmationHarness() {
  const { confirm, dialog } = useConfirm();
  const [result, setResult] = useState<boolean | null>(null);
  return (
    <>
      <button
        type="button"
        onClick={() =>
          void confirm({
            title: "Delete all data?",
            confirmLabel: "Delete all data",
            destructive: true,
            requireText: "delete all data",
          }).then(setResult)
        }
      >
        Ask
      </button>
      {result !== null && <output>{String(result)}</output>}
      {dialog}
    </>
  );
}

it("keeps a typed confirmation disabled until the exact phrase is typed", async () => {
  const user = userEvent.setup();
  render(<TypedConfirmationHarness />);

  await user.click(screen.getByRole("button", { name: "Ask" }));
  const confirmButton = await screen.findByRole("button", {
    name: "Delete all data",
  });
  expect(confirmButton).toBeDisabled();

  const field = screen.getByLabelText("Type delete all data to confirm.");
  await user.type(field, "delete all");
  expect(confirmButton).toBeDisabled();
  await user.type(field, " data");
  expect(confirmButton).toBeEnabled();

  await user.click(confirmButton);
  await waitFor(() => expect(screen.getByText("true")).toBeVisible());
});
