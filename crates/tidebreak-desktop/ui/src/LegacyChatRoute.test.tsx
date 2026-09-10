// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { LegacyChatRoute } from "./LegacyChatRoute";
const mocks = vi.hoisted(() => ({
  client: { getCodeSession: vi.fn() },
  navigate: vi.fn(),
}));
vi.mock("./AppContext", () => ({ useApp: () => ({ client: mocks.client }) }));
vi.mock("@tanstack/react-router", () => ({
  useNavigate: () => mocks.navigate,
}));
vi.mock("./ChatRoute", () => ({
  ChatRoute: ({ chatId }: { chatId: string }) => <p>Owner chat {chatId}</p>,
}));
afterEach(cleanup);
beforeEach(() => {
  mocks.client.getCodeSession.mockReset();
  mocks.navigate.mockReset();
});
it("redirects an authorized shared legacy link before owner chat mounts", async () => {
  mocks.client.getCodeSession.mockResolvedValue({
    id: "shared",
    is_owner: false,
  });
  render(<LegacyChatRoute chatId="shared" />);
  await waitFor(() =>
    expect(mocks.navigate).toHaveBeenCalledWith({
      to: "/code/s/$sessionId",
      params: { sessionId: "shared" },
      replace: true,
    }),
  );
  expect(screen.queryByText("Owner chat shared")).toBeNull();
});
it.each([true, undefined])(
  "keeps an owner or legacy session on the normal chat route",
  async (is_owner) => {
    mocks.client.getCodeSession.mockResolvedValue({ id: "owned", is_owner });
    render(<LegacyChatRoute chatId="owned" />);
    expect(await screen.findByText("Owner chat owned")).toBeTruthy();
    expect(mocks.navigate).not.toHaveBeenCalled();
  },
);
it("does not turn a failed access lookup into shared access", async () => {
  mocks.client.getCodeSession.mockRejectedValue(new Error("not found"));
  render(<LegacyChatRoute chatId="private" />);
  expect(await screen.findByText("Owner chat private")).toBeTruthy();
  expect(mocks.navigate).not.toHaveBeenCalled();
});
