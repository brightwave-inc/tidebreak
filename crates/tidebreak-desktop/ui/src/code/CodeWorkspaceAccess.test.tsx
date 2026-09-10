// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { codeSession, codeWorkspace } from "@/stories/fixtures";
import { CodeWorkspacePage } from "./CodeWorkspacePage";

const mocks = vi.hoisted(() => {
  const client = {
    getCodeWorkspace: vi.fn(),
    listCodeWorkspaceSessions: vi.fn(),
    getCodeRepo: vi.fn(),
    getCodeWorkspacePr: vi.fn(),
    listCodeWorkspaceFiles: vi.fn(),
    getCodeWorkspaceDiff: vi.fn(),
    openCodeTerminal: vi.fn(),
  };
  return { client, search: { task: "second" } };
});
vi.mock("@/AppContext", () => ({
  useApp: () => ({ client: mocks.client, models: [], defaultModelKey: null }),
}));
vi.mock("@tanstack/react-router", () => ({
  useSearch: () => mocks.search,
  useNavigate: () => vi.fn(),
}));
vi.mock("@/RouteFrame", () => ({
  RouteFrame: ({ children }: { children: React.ReactNode }) => children,
}));
vi.mock("./CodeSidebar", () => ({ CodeSidebar: () => null }));
vi.mock("./CodeSessionPage", () => ({
  CodeSessionContent: ({
    session,
    error,
    title,
  }: {
    session: { id: string } | null;
    error: string | null;
    title: string;
  }) => (
    <div>
      {title}
      {error ? (
        <p role="alert">{error}</p>
      ) : session ? (
        <p>Session {session.id}</p>
      ) : (
        <p>Opening</p>
      )}
    </div>
  ),
}));
afterEach(cleanup);
beforeEach(() => {
  for (const fn of Object.values(mocks.client)) fn.mockReset();
  mocks.search.task = "second";
});
it("opens the exact shared task without mounting owner workspace APIs", async () => {
  mocks.client.getCodeWorkspace.mockResolvedValue({
    ...codeWorkspace,
    read_only: true,
  });
  mocks.client.listCodeWorkspaceSessions.mockResolvedValue([
    { ...codeSession, id: "first" },
    { ...codeSession, id: "second", access: "view", is_owner: false },
  ]);
  render(<CodeWorkspacePage workspaceId={codeWorkspace.id} />);
  expect(await screen.findByText("Session second")).toBeTruthy();
  expect(mocks.client.getCodeWorkspace).toHaveBeenCalledTimes(1);
  expect(mocks.client.listCodeWorkspaceSessions).toHaveBeenCalledTimes(1);
  for (const name of [
    "getCodeRepo",
    "getCodeWorkspacePr",
    "listCodeWorkspaceFiles",
    "getCodeWorkspaceDiff",
    "openCodeTerminal",
  ] as const)
    expect(mocks.client[name]).not.toHaveBeenCalled();
});
it("does not fall back to another shared task when the named task is unavailable", async () => {
  mocks.client.getCodeWorkspace.mockResolvedValue({
    ...codeWorkspace,
    read_only: true,
  });
  mocks.client.listCodeWorkspaceSessions.mockResolvedValue([
    { ...codeSession, id: "first" },
  ]);
  render(<CodeWorkspacePage workspaceId={codeWorkspace.id} />);
  await waitFor(() =>
    expect(screen.getByRole("alert").textContent).toContain("unavailable"),
  );
  expect(screen.queryByText("Session first")).toBeNull();
});
