// @vitest-environment jsdom
import { fireEvent, render, waitFor } from "@testing-library/react";
import { useRef } from "react";
import { beforeEach, expect, it, vi } from "vitest";

const hostMocks = vi.hoisted(() => ({ openExternal: vi.fn() }));
vi.mock("@/host", () => ({ openExternal: hostMocks.openExternal }));

import { useSecureViewerLinks } from "./extendViewerSurface";

function LinkSurface() {
  const ref = useRef<HTMLDivElement>(null);
  useSecureViewerLinks(ref);
  return (
    <div ref={ref}>
      <a href="https://example.com/report">safe</a>
      <a href="http://example.com/plain">unsafe</a>
      <a href="https://user:secret@example.com/private">credentialed</a>
      <a href="javascript:alert('nope')">script</a>
      <a href="#section-2">bookmark</a>
    </div>
  );
}

beforeEach(() => {
  hostMocks.openExternal.mockReset();
  hostMocks.openExternal.mockResolvedValue(true);
});

it("keeps bookmarks internal and allows only host-opened HTTPS links", async () => {
  const { getByText } = render(<LinkSurface />);
  const safe = getByText("safe");
  const unsafe = getByText("unsafe");
  const credentialed = getByText("credentialed");
  const script = getByText("script");
  const bookmark = getByText("bookmark");

  await waitFor(() => expect(unsafe).not.toHaveAttribute("href"));
  expect(credentialed).not.toHaveAttribute("href");
  expect(script).not.toHaveAttribute("href");
  expect(safe).toHaveAttribute("target", "_blank");
  expect(safe).toHaveAttribute("rel", "noopener noreferrer");
  expect(bookmark).toHaveAttribute("href", "#section-2");

  fireEvent.click(safe);
  await waitFor(() =>
    expect(hostMocks.openExternal).toHaveBeenCalledWith(
      "https://example.com/report",
    ),
  );

  fireEvent.click(credentialed);
  fireEvent.click(script);
  expect(hostMocks.openExternal).toHaveBeenCalledTimes(1);
});
