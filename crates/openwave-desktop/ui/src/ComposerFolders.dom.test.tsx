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
            status: "connected",
            statements: (["read_files", "write_files"] as const).map(
              (capability, index) => ({
                handle: {
                  kind: "capability_grant" as const,
                  grant_id: `grant-${index}`,
                },
                level: { level: "chat" as const, chat_id: "chat-1" },
                level_title: null,
                verb: { kind: "capability" as const, capability },
                resource: {
                  kind: "host_root" as const,
                  root_id: "root-1",
                  display_name: null,
                },
                method: "folder_picker" as const,
                granted_at: "2026-07-30T12:00:00Z",
              }),
            ),
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
