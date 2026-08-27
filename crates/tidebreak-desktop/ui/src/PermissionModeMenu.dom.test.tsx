// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { toast } from "sonner";
import { afterEach, expect, it, vi } from "vitest";
import type { ManagedPolicy, PermissionMode } from "./api";
import { ManagedPolicyContext } from "./managedPolicy";
import { clampPermissionMode, PermissionModeMenu } from "./PermissionModeMenu";
import { useFirstTaskGuide } from "./FirstTaskWalkthrough";

vi.mock("sonner", () => ({ toast: { error: vi.fn() } }));

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  useFirstTaskGuide.getState().setSurface(null);
});

/**
 * The menu used to hold itself open after a choice, which reads as a dropped
 * click on a control whose whole job is one decision.
 */
it("applies the chosen mode and closes", async () => {
  const onChange = vi.fn();
  render(
    <PermissionModeMenu scopeKey="chat-1" value={null} onChange={onChange} />,
  );

  await userEvent.click(
    screen.getByRole("button", { name: "Permissions: Ask" }),
  );
  expect(screen.queryByText(/without asking/)).not.toBeInTheDocument();
  expect(screen.queryByText(/read-only/)).not.toBeInTheDocument();
  await userEvent.click(
    await screen.findByRole("menuitem", { name: /Allow all/ }),
  );

  expect(onChange).toHaveBeenCalledWith("allow");
  await waitFor(() =>
    expect(
      screen.queryByRole("menuitem", { name: /Allow all/ }),
    ).not.toBeInTheDocument(),
  );
});

/**
 * #923: a managed permission-mode ceiling renders over-ceiling modes as
 * locked — visible but unselectable — and a stored mode above the ceiling
 * displays as the ceiling the turn actually runs under.
 */
it("locks modes above a managed ceiling", async () => {
  const capped: ManagedPolicy = {
    managed: false,
    source: "unmanaged",
    misconfigured: false,
    allow_local_mcp_servers: false,
    permission_mode_ceiling: "ask",
  };
  const onChange = vi.fn();
  render(
    <ManagedPolicyContext.Provider value={capped}>
      <PermissionModeMenu scopeKey="chat-1" value="allow" onChange={onChange} />
    </ManagedPolicyContext.Provider>,
  );

  // The over-ceiling stored mode reads as its clamped effective mode.
  await userEvent.click(
    screen.getByRole("button", { name: "Permissions: Ask" }),
  );
  const locked = await screen.findByRole("menuitem", { name: /Allow all/ });
  expect(locked).toHaveAttribute("aria-disabled", "true");
  expect(locked).toHaveTextContent("Locked by your organization's policy.");
  expect(screen.getByRole("menuitem", { name: /Plan/ })).not.toHaveAttribute(
    "aria-disabled",
    "true",
  );

  await userEvent.click(locked);
  expect(onChange).not.toHaveBeenCalled();
});

it("clamps a requested mode down to a managed ceiling", () => {
  expect(clampPermissionMode("allow", "ask")).toBe("ask");
  expect(clampPermissionMode("auto", "ask")).toBe("ask");
  expect(clampPermissionMode("ask", "ask")).toBe("ask");
  expect(clampPermissionMode("plan", "ask")).toBe("plan");
  expect(clampPermissionMode("allow", null)).toBe("allow");
  expect(clampPermissionMode("allow", undefined)).toBe("allow");
});

/**
 * A code session has no turn-gate clamp, so the picker shows the stored
 * mode the engine launched with rather than the ceiling the chat menu
 * would display.
 */
it("shows a stored over-ceiling mode when display clamping is off", async () => {
  const capped: ManagedPolicy = {
    managed: false,
    source: "unmanaged",
    misconfigured: false,
    allow_local_mcp_servers: false,
    permission_mode_ceiling: "ask",
  };
  render(
    <ManagedPolicyContext.Provider value={capped}>
      <PermissionModeMenu
        scopeKey="code-1"
        value="allow"
        clampDisplay={false}
        onChange={vi.fn()}
      />
    </ManagedPolicyContext.Provider>,
  );

  expect(
    screen.getByRole("button", { name: "Permissions: Allow all" }),
  ).toBeInTheDocument();
});

it("lets a new chat save while an old chat write is still settling", async () => {
  let resolveOld!: () => void;
  const oldWrite = new Promise<void>((resolve) => {
    resolveOld = resolve;
  });
  const onChange = vi
    .fn<(mode: PermissionMode) => Promise<void>>()
    .mockImplementationOnce(() => oldWrite)
    .mockResolvedValueOnce(undefined);
  const { rerender } = render(
    <PermissionModeMenu scopeKey="chat-1" value="ask" onChange={onChange} />,
  );

  await userEvent.click(
    screen.getByRole("button", { name: "Permissions: Ask" }),
  );
  await userEvent.click(
    await screen.findByRole("menuitem", { name: /Allow all/ }),
  );
  expect(
    screen.getByRole("button", { name: "Permissions: Ask" }),
  ).toBeDisabled();

  rerender(
    <PermissionModeMenu scopeKey="chat-2" value="ask" onChange={onChange} />,
  );
  await waitFor(() =>
    expect(
      screen.getByRole("button", { name: "Permissions: Ask" }),
    ).not.toBeDisabled(),
  );

  await userEvent.click(
    screen.getByRole("button", { name: "Permissions: Ask" }),
  );
  await userEvent.click(await screen.findByRole("menuitem", { name: /Plan/ }));
  expect(onChange).toHaveBeenLastCalledWith("plan");

  resolveOld();
  await waitFor(() => expect(onChange).toHaveBeenCalledTimes(2));
});

it("does not toast a failed write after keyed chat navigation unmounts it", async () => {
  let rejectOld!: (reason: Error) => void;
  const oldWrite = new Promise<void>((_resolve, reject) => {
    rejectOld = reject;
  });
  const onChange = vi.fn().mockReturnValue(oldWrite);
  const oldChat = render(
    <PermissionModeMenu scopeKey="chat-1" value="ask" onChange={onChange} />,
  );

  await userEvent.click(
    screen.getByRole("button", { name: "Permissions: Ask" }),
  );
  await userEvent.click(
    await screen.findByRole("menuitem", { name: /Allow all/ }),
  );
  oldChat.unmount();
  render(
    <PermissionModeMenu scopeKey="chat-2" value="ask" onChange={vi.fn()} />,
  );

  await act(async () => rejectOld(new Error("old chat write failed")));

  expect(toast.error).not.toHaveBeenCalled();
  expect(
    screen.getByRole("button", { name: "Permissions: Ask" }),
  ).toBeEnabled();
});

it("opens when the first-task walkthrough is on the permissions step", () => {
  useFirstTaskGuide.getState().setSurface("permissions");
  render(
    <PermissionModeMenu scopeKey="chat-1" value="ask" onChange={vi.fn()} />,
  );

  expect(screen.getByRole("menuitem", { name: /Ask/ })).toHaveAttribute(
    "data-first-task-target",
    "permissions-ask",
  );
  expect(screen.getByRole("menuitem", { name: /Plan/ })).toBeInTheDocument();
});
