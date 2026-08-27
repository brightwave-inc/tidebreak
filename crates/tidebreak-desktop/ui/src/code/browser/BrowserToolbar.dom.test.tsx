// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type {
  BrowserHostSnapshot,
  BrowserProfileResetPhase,
} from "./browserHost";
import type { BrowserSession } from "./browserSession";
import { BrowserToolbar } from "./BrowserToolbar";

const baseEngine: NonNullable<BrowserHostSnapshot["engine"]> = {
  name: "wk_webview",
  capabilities: {
    lifecycle: true,
    persistentProfile: true,
    semanticSnapshot: false,
    semanticActions: false,
    screenshot: false,
    crossOriginFrames: false,
    profileReset: false,
  },
};

const resetEngine: NonNullable<BrowserHostSnapshot["engine"]> = {
  ...baseEngine,
  capabilities: {
    ...baseEngine.capabilities,
    profileReset: true,
  },
};

const session: BrowserSession = {
  version: 1,
  id: "browser-1",
  workspaceId: "workspace-1",
  url: "https://example.com/app",
  address: "example.com/app",
  title: "Example app",
  loadState: "ready",
  error: null,
  notice: null,
  inspectEnabled: false,
  history: [{ url: "https://example.com/app", title: "Example app" }],
  historyIndex: 0,
  updatedAt: Date.parse("2026-08-27T12:00:00.000Z"),
};

function toolbarProps(
  engine: BrowserHostSnapshot["engine"],
  options: {
    onOpenExternal?: () => void;
    onResetProfile?: () => Promise<void>;
    onOverlayOpenChange?: (open: boolean) => void;
    profileResetPhase?: BrowserProfileResetPhase;
  } = {},
) {
  return {
    session,
    address: session.address,
    addressError: null,
    canGoBack: false,
    canGoForward: false,
    engine,
    onAddressChange: vi.fn(),
    onNavigate: vi.fn(),
    onBack: vi.fn(),
    onForward: vi.fn(),
    onReload: vi.fn(),
    onStop: vi.fn(),
    onSelectHistory: vi.fn(),
    onOpenExternal: options.onOpenExternal ?? vi.fn(),
    onResetProfile:
      options.onResetProfile ??
      (engine?.capabilities.profileReset
        ? vi.fn(async () => undefined)
        : undefined),
    onOverlayOpenChange: options.onOverlayOpenChange ?? vi.fn(),
    profileResetPhase: options.profileResetPhase,
  };
}

async function openResetConfirmation(user: ReturnType<typeof userEvent.setup>) {
  await user.click(screen.getByRole("button", { name: "Browser options" }));
  await user.click(
    await screen.findByRole("menuitem", {
      name: "Reset development profile",
    }),
  );
  await screen.findByRole("alertdialog");
}

afterEach(cleanup);

describe("BrowserToolbar managed profile reset", () => {
  it("keeps direct external-open and hides reset when native support is absent", async () => {
    const user = userEvent.setup();
    const onOpenExternal = vi.fn();
    render(
      <BrowserToolbar {...toolbarProps(baseEngine, { onOpenExternal })} />,
    );

    expect(
      screen.queryByRole("button", { name: "Browser options" }),
    ).toBeNull();
    await user.click(screen.getByRole("button", { name: "Open externally" }));
    expect(onOpenExternal).toHaveBeenCalledOnce();
  });

  it("does not offer reset without the production owner callback", () => {
    render(
      <BrowserToolbar
        {...toolbarProps(resetEngine)}
        onResetProfile={undefined}
      />,
    );

    expect(
      screen.queryByRole("button", { name: "Browser options" }),
    ).toBeNull();
    expect(
      screen.getByRole("button", { name: "Open externally" }),
    ).toBeVisible();
  });

  it("explains the exact Tidebreak-only reset boundary before approval", async () => {
    const user = userEvent.setup();
    render(<BrowserToolbar {...toolbarProps(resetEngine)} />);

    await openResetConfirmation(user);

    expect(
      screen.getByRole("heading", { name: "Reset development profile?" }),
    ).toBeVisible();
    expect(
      screen.getByText(
        /same stored Tidebreak browser pages will return signed out in a fresh Tidebreak development profile/i,
      ),
    ).toBeVisible();
    expect(
      screen.getByText(
        /Managed cookies, site data, and cache are deleted; Safari and Chrome are untouched/i,
      ),
    ).toBeVisible();
    expect(
      screen.getByText(/existing agent origin grants remain/i),
    ).toBeVisible();
  });

  it("does not reset when the destructive confirmation is canceled", async () => {
    const user = userEvent.setup();
    const onResetProfile = vi.fn(async () => undefined);
    const onOverlayOpenChange = vi.fn();
    render(
      <BrowserToolbar
        {...toolbarProps(resetEngine, {
          onResetProfile,
          onOverlayOpenChange,
        })}
      />,
    );

    await openResetConfirmation(user);
    await user.click(screen.getByRole("button", { name: "Keep profile" }));

    expect(onResetProfile).not.toHaveBeenCalled();
    await waitFor(() =>
      expect(onOverlayOpenChange).toHaveBeenLastCalledWith(false),
    );
  });

  it("shows each native phase and keeps the browser obscured until reset settles", async () => {
    const user = userEvent.setup();
    let resolveReset: (() => void) | undefined;
    const onResetProfile = vi.fn(
      () =>
        new Promise<void>((resolve) => {
          resolveReset = resolve;
        }),
    );
    const onOverlayOpenChange = vi.fn();
    const props = toolbarProps(resetEngine, {
      onResetProfile,
      onOverlayOpenChange,
    });
    const view = render(<BrowserToolbar {...props} />);

    await openResetConfirmation(user);
    await user.click(
      screen.getByRole("button", { name: "Reset development profile" }),
    );

    expect(
      await screen.findByText(
        "Resetting the Tidebreak development profile… closing browser tabs.",
      ),
    ).toBeVisible();
    expect(
      screen.getByRole("button", { name: "Resetting development profile" }),
    ).toBeDisabled();
    expect(onOverlayOpenChange).toHaveBeenLastCalledWith(true);

    view.rerender(<BrowserToolbar {...props} profileResetPhase="deleting" />);
    expect(
      await screen.findByText(
        "Resetting the Tidebreak development profile… deleting managed cookies, site data, and cache.",
      ),
    ).toBeVisible();

    view.rerender(
      <BrowserToolbar {...props} profileResetPhase="reconstructing" />,
    );
    expect(
      await screen.findByText(
        "Resetting the Tidebreak development profile… reopening stored browser pages.",
      ),
    ).toBeVisible();

    view.rerender(<BrowserToolbar {...props} profileResetPhase={null} />);
    expect(
      screen.getByText(
        "Resetting the Tidebreak development profile… reopening stored browser pages.",
      ),
    ).toBeVisible();

    await act(async () => resolveReset?.());

    await waitFor(() =>
      expect(
        screen.queryByRole("button", { name: "Resetting development profile" }),
      ).toBeNull(),
    );
    expect(onOverlayOpenChange).toHaveBeenLastCalledWith(false);
  });

  it("shows a sibling reset phase and prevents a competing reset", () => {
    render(
      <BrowserToolbar
        {...toolbarProps(resetEngine)}
        profileResetPhase="deleting"
      />,
    );

    expect(
      screen.getByText(
        "Resetting the Tidebreak development profile… deleting managed cookies, site data, and cache.",
      ),
    ).toBeVisible();
    expect(
      screen.getByRole("button", { name: "Resetting development profile" }),
    ).toBeDisabled();
  });

  it("presents a native failure with retry and dismissal", async () => {
    const user = userEvent.setup();
    const onResetProfile = vi
      .fn<() => Promise<void>>()
      .mockRejectedValueOnce(new Error("WebKit could not remove profile data"))
      .mockResolvedValueOnce(undefined);
    render(
      <BrowserToolbar {...toolbarProps(resetEngine, { onResetProfile })} />,
    );

    await openResetConfirmation(user);
    await user.click(
      screen.getByRole("button", { name: "Reset development profile" }),
    );

    expect(
      await screen.findByText("WebKit could not remove profile data"),
    ).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Try again" }));
    await user.click(
      await screen.findByRole("button", {
        name: "Reset development profile",
      }),
    );
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Browser options" }),
      ).toBeEnabled(),
    );
    expect(onResetProfile).toHaveBeenCalledTimes(2);
    expect(
      screen.queryByText("WebKit could not remove profile data"),
    ).toBeNull();

    onResetProfile.mockRejectedValueOnce(new Error("Profile is still busy"));
    await openResetConfirmation(user);
    await user.click(
      screen.getByRole("button", { name: "Reset development profile" }),
    );
    expect(await screen.findByText("Profile is still busy")).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Dismiss" }));
    expect(screen.queryByText("Profile is still busy")).toBeNull();
  });

  it("releases obscuration if reset support disappears with the menu open", async () => {
    const user = userEvent.setup();
    const onOverlayOpenChange = vi.fn();
    const props = toolbarProps(resetEngine, { onOverlayOpenChange });
    const view = render(<BrowserToolbar {...props} />);

    await user.click(screen.getByRole("button", { name: "Browser options" }));
    expect(onOverlayOpenChange).toHaveBeenLastCalledWith(true);

    view.rerender(<BrowserToolbar {...props} engine={baseEngine} />);

    await waitFor(() =>
      expect(onOverlayOpenChange).toHaveBeenLastCalledWith(false),
    );
    expect(
      screen.queryByRole("button", { name: "Browser options" }),
    ).toBeNull();
    expect(
      screen.getByRole("button", { name: "Open externally" }),
    ).toBeVisible();
  });
});
