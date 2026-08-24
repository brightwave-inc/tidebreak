// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";

import { Composer, type ComposerImages } from "./Composer";
import { queuedImageAttachment } from "./ImageAttachments";

afterEach(cleanup);

const noop = async () => undefined;

function images(count: number): ComposerImages {
  return {
    items: Array.from({ length: count }, (_, index) =>
      queuedImageAttachment(`image-${index}`, {
        name: `context-${index + 1}.png`,
        byteLen: 1_000,
        previewUrl: `blob:image-${index}`,
      }),
    ),
    error: null,
    unsupportedModel: null,
    onAttachFiles: vi.fn(),
    onRemove: vi.fn(),
    onRetry: vi.fn(),
  };
}

function composerProps(imageCount: number) {
  return {
    activeTurnId: null,
    busy: false,
    cancelError: null,
    cancelPending: false,
    disabled: false,
    draft: "",
    images: images(imageCount),
    onDraftChange: vi.fn(),
    onSend: noop,
    onSteer: noop,
    onStop: noop,
    resetKey: "chat-1",
    steerError: null,
    steerPending: false,
    steerStatus: null,
  };
}

it("collapses when added context first crosses the dense threshold", async () => {
  const user = userEvent.setup();
  const view = render(<Composer {...composerProps(3)} />);

  expect(
    screen.getByRole("button", {
      name: /Context 3 3 images Hide details/i,
    }),
  ).toBeVisible();
  expect(
    screen.getByRole("button", {
      name: /Context 3 3 images Hide details/i,
    }),
  ).toHaveAttribute("aria-expanded", "true");
  expect(screen.getByLabelText("Remove context-1.png")).toBeVisible();

  view.rerender(<Composer {...composerProps(4)} />);

  expect(
    await screen.findByRole("button", {
      name: /Context 4 4 images Show details/i,
    }),
  ).toBeVisible();
  expect(
    screen.getByRole("button", {
      name: /Context 4 4 images Show details/i,
    }),
  ).toHaveAttribute("aria-expanded", "false");
  expect(screen.queryByLabelText("Remove context-1.png")).toBeNull();

  await user.click(
    screen.getByRole("button", {
      name: /Context 4 4 images Show details/i,
    }),
  );
  view.rerender(<Composer {...composerProps(5)} />);

  expect(
    screen.getByRole("button", {
      name: /Context 5 5 images Hide details/i,
    }),
  ).toBeVisible();
  expect(
    screen.getByRole("button", {
      name: /Context 5 5 images Hide details/i,
    }),
  ).toHaveAttribute("aria-expanded", "true");
  expect(screen.getByLabelText("Remove context-5.png")).toBeVisible();
});

it("keeps the compact footer focus order aligned with its two visual rows", () => {
  render(
    <Composer
      {...composerProps(0)}
      activeTurnId="turn-1"
      busy
      draft="Queue this next"
      files={{
        items: [],
        attaching: false,
        onAttach: vi.fn(),
        onRemove: vi.fn(),
      }}
      modelMenu={<button type="button">Model</button>}
      permissionMenu={<button type="button">Permissions</button>}
      contextUsage={{
        contextTokens: 64_000,
        spend: {
          input: 40_000,
          output: 2_000,
          cacheRead: 22_000,
          cacheWrite: 0,
        },
        contextWindow: 128_000,
        modelName: "Review model",
      }}
      onQueue={noop}
    />,
  );

  const order = screen
    .getAllByRole("button")
    .map((button) => button.getAttribute("aria-label") ?? button.textContent);

  expect(order).toEqual([
    "Tools",
    "Model",
    "Permissions",
    "Context: 50% of 128k tokens used",
    "Queue message for after this response",
    "Stop response",
  ]);
});
