// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";

import { Composer } from "./Composer";

afterEach(cleanup);

it("drops folder chips after send without needing a disconnect", () => {
  render(
    <Composer
      activeTurnId="turn-1"
      busy
      cancelError={null}
      cancelPending={false}
      disabled={false}
      draft=""
      folders={{
        items: [
          {
            rootId: "root-1",
            displayName: "Downloads",
            status: "connected",
            availableInFutureChats: true,
            statements: (
              ["read_files", "write_files", "execute_commands"] as const
            ).map((capability, index) => ({
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
            })),
          },
        ],
        pendingIds: [],
        working: false,
        error: null,
        onRemove: vi.fn(),
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

  expect(screen.queryByText("Downloads")).toBeNull();
  expect(screen.queryByLabelText("Attached folders")).toBeNull();
});
