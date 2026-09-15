import type { Meta, StoryObj } from "@storybook/react-vite";
import { userEvent, within } from "storybook/test";
import { HttpError } from "@/api/client";
import {
  bulkArchiveDiscardConfirmation,
  type WorkspaceArchiveFailure,
} from "@/code/bulkWorkspaceArchive";
import { useConfirm } from "@/components/ConfirmDialog";
import { Button } from "@/components/ui/button";
import { codeWorkspace } from "./fixtures";

function ArchiveConfirmation({
  blocked,
}: {
  blocked: WorkspaceArchiveFailure[];
}) {
  const { confirm, dialog } = useConfirm();
  return (
    <>
      <Button
        onClick={() => void confirm(bulkArchiveDiscardConfirmation(blocked))}
      >
        Review leftover work
      </Button>
      {dialog}
    </>
  );
}

const blocked: WorkspaceArchiveFailure[] = [
  {
    workspace: { ...codeWorkspace, title: "Fix login redirects" },
    error: new HttpError(
      409,
      "409: Workspace has uncommitted changes; pass force to discard them",
      "uncommitted",
    ),
  },
  {
    workspace: {
      ...codeWorkspace,
      id: "ws-2",
      title: "Update account settings",
    },
    error: new HttpError(
      409,
      "409: Workspace contains ignored files",
      "ignored_content",
    ),
  },
];

const meta = {
  title: "Code/Bulk workspace archive",
  component: ArchiveConfirmation,
  parameters: { layout: "fullscreen" },
  args: { blocked },
  play: async ({ canvasElement }) => {
    await userEvent.click(
      within(canvasElement).getByRole("button", {
        name: "Review leftover work",
      }),
    );
    await within(canvasElement.ownerDocument.body).findByRole("alertdialog");
  },
} satisfies Meta<typeof ArchiveConfirmation>;
export default meta;
type Story = StoryObj<typeof meta>;

export const MultipleBlocked: Story = {};
export const OneBlocked: Story = { args: { blocked: blocked.slice(0, 1) } };
export const LongSelection: Story = {
  args: {
    blocked: Array.from({ length: 12 }, (_, index) => ({
      workspace: {
        ...codeWorkspace,
        id: `ws-${index}`,
        title: `${index + 1}. Preserve workspace drafts and restore the previous editor selection after reconnecting`,
      },
      error: new HttpError(
        409,
        "409: Workspace has uncommitted changes; pass force to discard them",
        "uncommitted",
      ),
    })),
  },
};
