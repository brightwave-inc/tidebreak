// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { CodeSessionPage } from "./CodeSessionPage";
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
  CodeSessionPane: ({ session }: { session: { id: string } }) => (
    <div>Session {session.id}</div>
  ),
}));

afterEach(cleanup);
beforeEach(() => {
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
