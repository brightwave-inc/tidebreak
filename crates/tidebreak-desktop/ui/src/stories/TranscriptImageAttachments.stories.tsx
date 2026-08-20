import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, userEvent, within } from "storybook/test";

import { TranscriptImageAttachments } from "@/TranscriptImageAttachments";

const PIXEL = new Blob(
  [
    Uint8Array.from(
      atob(
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==",
      ),
      (character) => character.charCodeAt(0),
    ),
  ],
  { type: "image/png" },
);

const images = [
  {
    attachmentId: "1c2f1a44-2f3b-4a1e-9f0a-2b6d5c4e3a21",
    mediaType: "image/png",
    width: 390,
    height: 202,
  },
  {
    attachmentId: "2d3e2b55-3a4c-4b2f-8e1b-3c7e6d5f4b32",
    mediaType: "image/png",
    width: 2102,
    height: 416,
  },
];

/**
 * Sent images on a user turn. Default is a small thumbnail; click opens or
 * closes a larger preview.
 */
const meta = {
  title: "Chat/Transcript images",
  component: TranscriptImageAttachments,
  args: {
    client: {
      getChatImageAttachment: async () => PIXEL,
    },
    chatId: "chat-1",
    images,
  },
  decorators: [
    (Story) => (
      <div className="bg-muted max-w-md rounded-xl p-3">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof TranscriptImageAttachments>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Thumbnails: Story = {};

export const Expanded: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const toggle = await canvas.findByRole("button", {
      name: "Expand attached image 1: 390 by 202 pixels",
    });
    await userEvent.click(toggle);
    await expect(toggle).toHaveAttribute("aria-expanded", "true");
  },
};

export const Unavailable: Story = {
  args: {
    client: {
      getChatImageAttachment: async () =>
        new Blob(["nope"], { type: "application/octet-stream" }),
    },
    images: [images[0]],
  },
};
