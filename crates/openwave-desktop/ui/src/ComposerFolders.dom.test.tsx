// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";

import { Composer } from "./Composer";

afterEach(cleanup);

it("attaches and confirms revoking a folder from the ordinary chat composer", async () => {
  const onAttach = vi.fn();
  const onRemove = vi.fn();
  const user = userEvent.setup();
  render(
    <Composer
      activeTurnId={null}
      busy={false}
      cancelError={null}
      cancelPending={false}
      disabled={false}
      draft=""
      folders={{
        items: [
          {
            rootId: "root-1",
            displayName: "Research",
            capabilities: ["read", "write"],
          },
        ],
        working: false,
        error: null,
        onAttach,
        onRemove,
      }}
      onDraftChange={vi.fn()}
      onSend={vi.fn(async () => {})}
      onSteer={vi.fn(async () => {})}
      onStop={vi.fn(async () => {})}
      resetKey="chat-1"
      steerError={null}
      steerPending={false}
      steerStatus={null}
    />,
  );

  await user.click(screen.getByRole("button", { name: "Attach folder" }));
  expect(onAttach).toHaveBeenCalledOnce();
  expect(screen.getByText("Read and write")).toBeInTheDocument();

  await user.click(
    screen.getByRole("button", { name: "Disconnect Research" }),
  );
  expect(onRemove).not.toHaveBeenCalled();
  await user.click(screen.getByRole("button", { name: "Disconnect" }));
  expect(onRemove).toHaveBeenCalledWith("root-1");
});
