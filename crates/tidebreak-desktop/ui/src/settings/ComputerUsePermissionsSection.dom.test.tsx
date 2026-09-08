// @vitest-environment jsdom
import {
  act,
  cleanup,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type {
  ComputerUsePermissionHost,
  ComputerUsePermissionStatus,
} from "@/computerUsePermissions";
import { ComputerUsePermissionsSection } from "./ComputerUsePermissionsSection";

const missing: ComputerUsePermissionStatus = {
  status: "available",
  appName: "WK Acceptance",
  appIdentifier: "io.brightwave.tidebreak.wkacceptance.test",
  accessibility: false,
  screenRecording: true,
};
function host(overrides: Partial<ComputerUsePermissionHost> = {}) {
  return {
    availability: () => "local" as const,
    status: vi.fn().mockResolvedValue(missing),
    request: vi.fn().mockResolvedValue({ ...missing, accessibility: true }),
    openSettings: vi.fn().mockResolvedValue(undefined),
    ...overrides,
  };
}

function pending<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: Error) => void;
  const promise = new Promise<T>((onResolve, onReject) => {
    resolve = onResolve;
    reject = onReject;
  });
  return { promise, resolve, reject };
}

afterEach(cleanup);

describe("ComputerUsePermissionsSection", () => {
  it("checks status without requesting permission and names the running app", async () => {
    const native = host();
    render(<ComputerUsePermissionsSection host={native} />);
    await screen.findByText(/WK Acceptance/);
    expect(native.status).toHaveBeenCalledTimes(1);
    expect(native.request).not.toHaveBeenCalled();
    expect(screen.getByText("Not allowed")).toBeInTheDocument();
    expect(screen.getByText("Allowed")).toBeInTheDocument();
    await userEvent.click(
      screen.getByRole("button", { name: "Request macOS permissions" }),
    );
    await screen.findByText(/macOS permissions are ready/);
    expect(native.request).toHaveBeenCalledTimes(1);
    expect(
      screen.queryByRole("button", { name: "Request macOS permissions" }),
    ).not.toBeInTheDocument();
  });

  it("opens only the pane selected by the user and refreshes on return", async () => {
    const native = host();
    render(<ComputerUsePermissionsSection host={native} />);
    await userEvent.click(
      await screen.findByRole("button", {
        name: "Open Accessibility settings",
      }),
    );
    expect(native.openSettings).toHaveBeenCalledWith("accessibility");
    await userEvent.click(
      screen.getByRole("button", { name: "Open Screen Recording settings" }),
    );
    expect(native.openSettings).toHaveBeenLastCalledWith("screen_recording");
    act(() => window.dispatchEvent(new Event("focus")));
    await waitFor(() => expect(native.status).toHaveBeenCalledTimes(2));
    expect(native.request).not.toHaveBeenCalled();
  });

  it("shows a status failure without claiming either permission is denied", async () => {
    const native = host({
      status: vi.fn().mockRejectedValue(new Error("broker unavailable")),
    });
    render(<ComputerUsePermissionsSection host={native} />);
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "permission status could not be checked",
    );
    expect(screen.queryByText("Not allowed")).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Request macOS permissions" }),
    ).not.toBeInTheDocument();
    expect(native.request).not.toHaveBeenCalled();
  });

  it("keeps the known status when an explicit request fails", async () => {
    const native = host({
      request: vi.fn().mockRejectedValue(new Error("request failed")),
    });
    render(<ComputerUsePermissionsSection host={native} />);
    await userEvent.click(
      await screen.findByRole("button", { name: "Request macOS permissions" }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "permissions could not be requested",
    );
    expect(screen.getByText("Allowed")).toBeInTheDocument();
    expect(screen.getByText("Not allowed")).toBeInTheDocument();
  });

  it.each(["remote", "web"] as const)(
    "does not reach the local helper from %s mode",
    async (availability) => {
      const native = host({ availability: () => availability });
      render(<ComputerUsePermissionsSection host={native} />);
      expect(screen.queryByRole("button")).not.toBeInTheDocument();
      act(() => window.dispatchEvent(new Event("focus")));
      expect(native.status).not.toHaveBeenCalled();
      expect(native.request).not.toHaveBeenCalled();
    },
  );

  it.each(["before", "after"] as const)(
    "checks status after a request when a stale focus read resolves %s it",
    async (focusOrder) => {
      const request = pending<ComputerUsePermissionStatus>();
      const focusRead = pending<ComputerUsePermissionStatus>();
      const native = host({
        status: vi
          .fn()
          .mockResolvedValueOnce(missing)
          .mockReturnValueOnce(focusRead.promise)
          .mockResolvedValue({ ...missing, accessibility: true }),
        request: vi.fn(() => request.promise),
      });
      render(<ComputerUsePermissionsSection host={native} />);
      await userEvent.click(
        await screen.findByRole("button", {
          name: "Request macOS permissions",
        }),
      );
      act(() => window.dispatchEvent(new Event("focus")));
      if (focusOrder === "before") {
        await act(async () => focusRead.resolve(missing));
      }
      await act(async () =>
        request.resolve({ ...missing, accessibility: true }),
      );
      await screen.findByText(/macOS permissions are ready/);
      if (focusOrder === "after") {
        await act(async () => focusRead.resolve(missing));
      }
      expect(native.status).toHaveBeenCalledTimes(3);
      expect(screen.queryByText("Not allowed")).not.toBeInTheDocument();
    },
  );

  it.each(["before", "after"] as const)(
    "keeps request errors when a focus read resolves %s the request fails",
    async (focusOrder) => {
      const request = pending<ComputerUsePermissionStatus>();
      const focusRead = pending<ComputerUsePermissionStatus>();
      const native = host({
        status: vi
          .fn()
          .mockResolvedValueOnce(missing)
          .mockReturnValueOnce(focusRead.promise),
        request: vi.fn(() => request.promise),
      });
      render(<ComputerUsePermissionsSection host={native} />);
      await userEvent.click(
        await screen.findByRole("button", {
          name: "Request macOS permissions",
        }),
      );
      act(() => window.dispatchEvent(new Event("focus")));
      if (focusOrder === "before") {
        await act(async () => focusRead.resolve(missing));
      }
      await act(async () => request.reject(new Error("request failed")));
      expect(await screen.findByRole("alert")).toHaveTextContent(
        "permissions could not be requested",
      );
      if (focusOrder === "after") {
        await act(async () => focusRead.resolve(missing));
      }
      expect(screen.getByRole("alert")).toHaveTextContent(
        "permissions could not be requested",
      );
      expect(screen.getByRole("button", { name: "Refresh" })).toBeEnabled();
    },
  );

  it("keeps a permission request result from replacing a later refresh", async () => {
    let resolve!: (status: ComputerUsePermissionStatus) => void;
    const readStatus = vi.fn().mockResolvedValue(missing);
    const native = host({
      status: readStatus,
      request: vi.fn(
        () =>
          new Promise<ComputerUsePermissionStatus>((next) => {
            resolve = next;
          }),
      ),
    });
    render(<ComputerUsePermissionsSection host={native} />);
    await userEvent.click(
      await screen.findByRole("button", { name: "Request macOS permissions" }),
    );
    readStatus.mockResolvedValue({ ...missing, accessibility: true });
    act(() => window.dispatchEvent(new Event("focus")));
    await screen.findByText(/macOS permissions are ready/);
    await act(async () => resolve(missing));
    expect(screen.queryByText("Not allowed")).not.toBeInTheDocument();
  });

  it("keeps a later focus result while the post-request read is pending", async () => {
    const request = pending<ComputerUsePermissionStatus>();
    const postRequestRead = pending<ComputerUsePermissionStatus>();
    const native = host({
      status: vi
        .fn()
        .mockResolvedValueOnce(missing)
        .mockResolvedValueOnce(missing)
        .mockReturnValueOnce(postRequestRead.promise)
        .mockResolvedValue({ ...missing, accessibility: true }),
      request: vi.fn(() => request.promise),
    });
    render(<ComputerUsePermissionsSection host={native} />);
    await userEvent.click(
      await screen.findByRole("button", { name: "Request macOS permissions" }),
    );
    await act(async () => window.dispatchEvent(new Event("focus")));
    await act(async () => request.resolve(missing));
    expect(native.status).toHaveBeenCalledTimes(3);
    act(() => window.dispatchEvent(new Event("focus")));
    await screen.findByText(/macOS permissions are ready/);
    await act(async () => postRequestRead.resolve(missing));
    expect(screen.queryByText("Not allowed")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Refresh" })).toBeEnabled();
  });

  it.each(["success", "failure"] as const)(
    "ignores an old host request's %s after the host changes",
    async (outcome) => {
      const request = pending<ComputerUsePermissionStatus>();
      const oldHost = host({ request: vi.fn(() => request.promise) });
      const replacement = host({
        status: vi.fn().mockResolvedValue({ ...missing, accessibility: true }),
      });
      const view = render(<ComputerUsePermissionsSection host={oldHost} />);
      await userEvent.click(
        await screen.findByRole("button", {
          name: "Request macOS permissions",
        }),
      );
      await act(async () => window.dispatchEvent(new Event("focus")));
      view.rerender(<ComputerUsePermissionsSection host={replacement} />);
      await screen.findByText(/macOS permissions are ready/);
      expect(screen.getByRole("button", { name: "Refresh" })).toBeEnabled();
      await act(async () => {
        if (outcome === "success") request.resolve(missing);
        else request.reject(new Error("old request failed"));
      });
      expect(oldHost.status).toHaveBeenCalledTimes(2);
      expect(replacement.status).toHaveBeenCalledTimes(1);
      expect(screen.queryByRole("alert")).not.toBeInTheDocument();
      expect(screen.queryByText("Not allowed")).not.toBeInTheDocument();
    },
  );

  it("keeps a late old status read from replacing a newer result", async () => {
    let resolve!: (status: ComputerUsePermissionStatus) => void;
    const native = host({
      status: vi
        .fn()
        .mockReturnValueOnce(
          new Promise<ComputerUsePermissionStatus>((next) => {
            resolve = next;
          }),
        )
        .mockResolvedValue({ ...missing, accessibility: true }),
    });
    render(<ComputerUsePermissionsSection host={native} />);
    act(() => window.dispatchEvent(new Event("focus")));
    await screen.findByText(/macOS permissions are ready/);
    await act(async () => resolve(missing));
    expect(screen.queryByText("Not allowed")).not.toBeInTheDocument();
    expect(
      within(
        screen.getByRole("region", { name: "Computer use on this Mac" }),
      ).getAllByText("Allowed"),
    ).toHaveLength(2);
  });
});
