import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, fn, within } from "storybook/test";

import { Composer } from "@/Composer";

function FileDropStory({ documents }: { documents: boolean }) {
  return (
    <div className="mx-auto w-full max-w-3xl p-4">
      <Composer
        activeTurnId={null}
        busy={false}
        cancelError={null}
        cancelPending={false}
        disabled={false}
        draft=""
        files={
          documents
            ? {
                items: [],
                attaching: false,
                onAttachHeld: fn(),
                onRemove: fn(),
              }
            : undefined
        }
        images={{
          items: [],
          error: null,
          unsupportedModel: null,
          onAttachFiles: fn(),
          onRemove: fn(),
          onRetry: fn(),
        }}
        onDraftChange={fn()}
        onSend={fn()}
        onSteer={fn()}
        onStop={fn()}
        resetKey="file-drop-story"
        steerError={null}
        steerPending={false}
        steerStatus={null}
      />
    </div>
  );
}

const meta = {
  title: "Composer/File drop",
  component: FileDropStory,
  parameters: { layout: "fullscreen" },
  play: async ({ canvasElement, args }) => {
    const canvas = within(canvasElement);
    const textbox = await canvas.findByRole("textbox", { name: "Message" });
    const transfer = new DataTransfer();
    transfer.items.add(
      new File(["# Notes"], args.documents ? "notes.md" : "shot.png", {
        type: args.documents ? "text/markdown" : "image/png",
      }),
    );
    textbox.dispatchEvent(
      new DragEvent("dragenter", { bubbles: true, dataTransfer: transfer }),
    );
    await expect(
      await canvas.findByText(
        args.documents
          ? "Drop files to attach them"
          : "Drop an image to attach it",
      ),
    ).toBeVisible();
  },
} satisfies Meta<typeof FileDropStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const HostedDocuments: Story = { args: { documents: true } };
export const CodeImages: Story = { args: { documents: false } };
