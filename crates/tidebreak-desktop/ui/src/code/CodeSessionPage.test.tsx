// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { CodeSessionPage, CodeSessionContent } from "./CodeSessionPage";
import { useCodeUpdatesStore } from "./CodeUpdatesStore";
import { codeSession } from "@/stories/fixtures";

const mocks = vi.hoisted(() => {
  const get = vi.fn();
  return { get, navigate: vi.fn(), client: { getCodeSession: get } };
});
vi.mock("@/AppContext", () => ({
  useApp: () => ({ client: mocks.client, models: [], defaultModelKey: null }),
}));
vi.mock("@tanstack/react-router", () => ({
  useNavigate: () => mocks.navigate,
}));
vi.mock("@/RouteFrame", () => ({
  RouteFrame: ({ children }: { children: React.ReactNode }) => children,
}));
vi.mock("./CodeSidebar", () => ({ CodeSidebar: () => null }));
vi.mock("./SessionLifecycleIndicator", () => ({
  SessionLifecycleIndicator: () => null,
}));
vi.mock("./workspace/CodeSessionPane", () => ({
  CodeSessionPane: ({
    session,
    disabled,
  }: {
    session: { id: string };
    disabled: boolean;
  }) => (
    <div data-testid="session-pane" data-disabled={disabled}>
      Session {session.id}
    </div>
  ),
}));

afterEach(cleanup);
beforeEach(() => {
  useCodeUpdatesStore.getState().resetLive();
  mocks.get.mockReset();
  mocks.navigate.mockReset();
});
describe("durable session links", () => {
  it("opens the named task in its workspace", async () => {
    mocks.get.mockResolvedValue({
      ...codeSession,
      id: "child",
      workspace_id: "workspace",
    });
    render(<CodeSessionPage sessionId="child" />);
    await waitFor(() =>
      expect(mocks.navigate).toHaveBeenCalledWith({
        to: "/code/w/$workspaceId",
        params: { workspaceId: "workspace" },
        search: { task: "child" },
        replace: true,
      }),
    );
  });
  it("renders a shared parent without owner-only chat requests", async () => {
    mocks.get.mockResolvedValue({
      ...codeSession,
      id: "parent",
      workspace_id: null,
      access: "view",
      is_owner: false,
    });
    render(<CodeSessionPage sessionId="parent" />);
    expect(await screen.findByText("Session parent")).toBeTruthy();
    expect(mocks.navigate).not.toHaveBeenCalled();
  });
  it("does not navigate after a denied session lookup", async () => {
    mocks.get.mockRejectedValue(new Error("Conversation unavailable"));
    render(<CodeSessionPage sessionId="private" />);
    expect(await screen.findByRole("alert")).toBeTruthy();
    expect(mocks.navigate).not.toHaveBeenCalled();
  });
  it("ignores a previous session lookup after navigation", async () => {
    let resolve!: (value: unknown) => void;
    mocks.get.mockImplementationOnce(
      () =>
        new Promise((done) => {
          resolve = done;
        }),
    );
    mocks.get.mockResolvedValueOnce({
      ...codeSession,
      id: "second",
      workspace_id: null,
    });
    const view = render(<CodeSessionPage sessionId="first" />);
    view.rerender(<CodeSessionPage sessionId="second" />);
    await screen.findByText("Session second");
    await act(async () =>
      resolve({ ...codeSession, id: "first", workspace_id: "old" }),
    );
    expect(mocks.navigate).not.toHaveBeenCalled();
    expect(screen.getByText("Session second")).toBeTruthy();
  });
});

it("updates workspace-less recovery from the digest without reloading its snapshot", () => {
  const session = {
    ...codeSession,
    workspace_id: null,
    lifecycle: "fenced" as const,
    fence_reason: { type: "orphan_alive" as const },
  };
  const { rerender } = render(
    <CodeSessionContent
      session={session}
      error={null}
      client={mocks.client as never}
      models={[]}
      defaultModelKey={null}
      onRetry={() => {}}
    />,
  );
  expect(screen.getByTestId("session-pane")).toHaveAttribute(
    "data-disabled",
    "true",
  );
  act(() =>
    useCodeUpdatesStore.getState().apply({
      type: "digest",
      digest: {
        workspace: null,
        session: session.id,
        kind: "interactive",
        lifecycle: "fenced",
        fence_reason: { type: "probe_ambiguous", detail: "Recovery stopped" },
        attention: {
          state: {
            type: "needs_you",
            prompt: "Inspect the previous process.",
            source: "lifecycle",
          },
          source: "lifecycle",
        },
        title: "Conversation",
        turn_count: 1,
      },
    }),
  );
  expect(screen.getByText("Inspect the previous process.")).toBeInTheDocument();
  act(() =>
    useCodeUpdatesStore.getState().apply({
      type: "digest",
      digest: {
        workspace: null,
        session: session.id,
        kind: "interactive",
        lifecycle: "idle",
        attention: { state: { type: "idle" }, source: "lifecycle" },
        title: "Conversation",
        turn_count: 1,
      },
    }),
  );
  expect(screen.getByTestId("session-pane")).toHaveAttribute(
    "data-disabled",
    "false",
  );
  expect(screen.queryByText("Inspect the previous process.")).toBeNull();
  rerender(
    <CodeSessionContent
      session={session}
      error={null}
      client={mocks.client as never}
      models={[]}
      defaultModelKey={null}
      onRetry={() => {}}
    />,
  );
  expect(screen.getByTestId("session-pane")).toHaveAttribute(
    "data-disabled",
    "false",
  );
});
