// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { AppFrame } from "./AppFrame";
import type { AppsApis } from "./appsApis";

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("AppFrame", () => {
  /**
   * The app bundle is untrusted markup served over http from the same
   * loopback origin the API answers on. `allow-same-origin` would put it on
   * that origin — able to read `server_info`, and through it the API bearer.
   * Nothing else in the renderer restates this, so the attribute is pinned
   * here.
   */
  it("runs a stored revision with an opaque origin", async () => {
    const apis = {
      baseUrl: "http://127.0.0.1:7777",
      viewSession: vi
        .fn()
        .mockResolvedValue({ frame_path: "/apps/view-frames/token-1" }),
      invoke: vi.fn(),
      invokeOperation: vi.fn(),
    } as unknown as AppsApis;

    render(<AppFrame appId="app-1" name="Budget" apis={apis} />);

    const frame = await screen.findByTitle("App: Budget");
    expect(frame).toHaveAttribute(
      "src",
      "http://127.0.0.1:7777/apps/view-frames/token-1",
    );
    expect(frame).toHaveAttribute("sandbox", "allow-scripts");
    expect(frame.getAttribute("sandbox")).not.toContain("allow-same-origin");
    expect(frame).toHaveAttribute("referrerPolicy", "no-referrer");
  });
});
