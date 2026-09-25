// @vitest-environment jsdom
import {
  act,
  cleanup,
  render,
  renderHook,
  screen,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ConnectionNotice } from "./ConnectionNotice";
import {
  CONNECTION_ESCALATE_AFTER_MS,
  machineHost,
  useConnectionIndicator,
  type SocketConnectionState,
} from "./connectionState";

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe("useConnectionIndicator", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  function follow(initial: SocketConnectionState) {
    return renderHook(({ state }) => useConnectionIndicator(state), {
      initialProps: { state: initial },
    });
  }

  it("keeps a short drop quiet and escalates one that lasts", () => {
    const { result, rerender } = follow("live");
    expect(result.current).toBe("live");

    rerender({ state: "reconnecting" });
    expect(result.current).toBe("reconnecting");
    act(() => vi.advanceTimersByTime(CONNECTION_ESCALATE_AFTER_MS - 1));
    expect(result.current).toBe("reconnecting");
    act(() => vi.advanceTimersByTime(1));
    expect(result.current).toBe("escalated");

    // Back online, the notice goes at once.
    rerender({ state: "live" });
    expect(result.current).toBe("live");
  });

  it("starts the wait over when a flapping connection comes back", () => {
    const { result, rerender } = follow("reconnecting");
    act(() => vi.advanceTimersByTime(CONNECTION_ESCALATE_AFTER_MS - 1_000));
    rerender({ state: "live" });
    rerender({ state: "reconnecting" });
    // The earlier drop's time does not count toward this one.
    act(() => vi.advanceTimersByTime(CONNECTION_ESCALATE_AFTER_MS - 1_000));
    expect(result.current).toBe("reconnecting");
    act(() => vi.advanceTimersByTime(1_000));
    expect(result.current).toBe("escalated");
  });
});

describe("ConnectionNotice", () => {
  it("says it is reconnecting without asking anything of the reader", () => {
    render(<ConnectionNotice state="reconnecting" onRetryNow={vi.fn()} />);
    expect(screen.getByRole("status")).toHaveTextContent(
      "Reconnecting to Tidebreak…",
    );
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
  });

  it("offers Retry now once the drop lasts, and waits for the attempt", async () => {
    const user = userEvent.setup();
    let settle = () => {};
    const onRetryNow = vi.fn(
      () =>
        new Promise<void>((resolve) => {
          settle = resolve;
        }),
    );
    render(<ConnectionNotice state="escalated" onRetryNow={onRetryNow} />);

    expect(screen.getByRole("status")).toHaveTextContent(
      "Tidebreak is not answering",
    );
    await user.click(screen.getByRole("button", { name: /Retry now/ }));
    expect(onRetryNow).toHaveBeenCalledOnce();
    // A second press cannot send a second attempt while the first runs.
    expect(screen.getByRole("button", { name: /Retry now/ })).toBeDisabled();
    await act(async () => settle());
    expect(screen.getByRole("button", { name: /Retry now/ })).toBeEnabled();
    // This computer's own server has no other machine to leave.
    expect(
      screen.queryByRole("button", { name: /Work on this computer/ }),
    ).not.toBeInTheDocument();
  });

  it("names another machine and offers the way back to this computer", async () => {
    const user = userEvent.setup();
    const onWorkLocally = vi.fn(async () => {});
    render(
      <ConnectionNotice
        state="escalated"
        machine="tidebreak.example.com"
        onRetryNow={vi.fn()}
        onWorkLocally={onWorkLocally}
      />,
    );

    expect(screen.getByRole("status")).toHaveTextContent(
      "tidebreak.example.com is not answering",
    );
    await user.click(
      screen.getByRole("button", { name: /Work on this computer/ }),
    );
    expect(onWorkLocally).toHaveBeenCalledOnce();
  });

  it("offers a restart when the local server stopped", async () => {
    const user = userEvent.setup();
    const onRestart = vi.fn(async () => {});
    render(
      <ConnectionNotice
        state="stopped"
        onRetryNow={vi.fn()}
        onRestart={onRestart}
      />,
    );

    expect(screen.getByRole("alert")).toHaveTextContent(
      "Tidebreak's server stopped",
    );
    expect(
      screen.queryByRole("button", { name: /Retry now/ }),
    ).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: /Restart Tidebreak/ }));
    expect(onRestart).toHaveBeenCalledOnce();
  });
});

describe("machineHost", () => {
  it("names a machine by its host", () => {
    expect(machineHost("https://tidebreak.example.com/api")).toBe(
      "tidebreak.example.com",
    );
    expect(machineHost("not a url")).toBe("not a url");
  });
});
