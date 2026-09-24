import type { Meta, StoryObj } from "@storybook/react-vite";
import { userEvent, within } from "storybook/test";

import { HttpError, type DataOverview } from "@/api";
import {
  DataPrivacyPanel,
  type DataPrivacyHost,
  type ExportableConversation,
} from "@/settings/DataPrivacyPanel";

const overview: DataOverview = {
  data_dir: "/Users/alex/Library/Application Support/io.brightwave.tidebreak",
  storage: "sqlite",
  usage: [
    { category: "database", bytes: 187_695_104 },
    { category: "attachments", bytes: 1_342_177_280 },
    { category: "outputs", bytes: 94_371_840 },
    { category: "logs", bytes: 12_582_912 },
    { category: "engine_tools", bytes: 1_073_741_824 },
    { category: "backups", bytes: 356_515_840 },
    { category: "other", bytes: 25_165_824 },
  ],
  total_bytes: 3_092_250_624,
};

const conversations: ExportableConversation[] = [
  {
    id: "9d5d84a0-6ba6-4c73-9e10-000000000001",
    title: "Launch plan for the October release",
    created_at: "2026-09-20T10:00:00Z",
  },
  {
    id: "9d5d84a0-6ba6-4c73-9e10-000000000002",
    title: "Quarterly revenue by region",
    created_at: "2026-09-18T15:30:00Z",
  },
  {
    id: "9d5d84a0-6ba6-4c73-9e10-000000000003",
    title: null,
    created_at: "2026-09-12T08:10:00Z",
  },
  {
    id: "9d5d84a0-6ba6-4c73-9e10-000000000004",
    title:
      "Draft the hiring rubric for the platform team, with calibration notes",
    created_at: "2026-09-02T17:45:00Z",
  },
];

function stubClient(options?: {
  overview?: DataOverview;
  pending?: boolean;
  fail?: boolean;
  member?: boolean;
}) {
  return {
    getDataOverview: () => {
      if (options?.pending) return new Promise<DataOverview>(() => {});
      if (options?.fail) {
        return Promise.reject(new Error("The server did not answer."));
      }
      if (options?.member) {
        return Promise.reject(
          new HttpError(403, "administrator access required"),
        );
      }
      return Promise.resolve(options?.overview ?? overview);
    },
    resetSettings: async () => ({}) as never,
    downloadProfileBackup: async () => {
      throw new Error("Stories do not download.");
    },
    downloadConversationExport: async () => {
      throw new Error("Stories do not download.");
    },
  };
}

function stubHost(options?: {
  local?: boolean;
  backupPending?: boolean;
}): DataPrivacyHost {
  return {
    local: options?.local ?? true,
    reveal: async () => {},
    saveBackup: () =>
      options?.backupPending
        ? new Promise(() => {})
        : Promise.resolve({
            path: "/Users/alex/Desktop/Tidebreak backup 2026-09-23.tar.gz",
            bytes: 1_610_612_736,
            count: 4_812,
          }),
    saveExport: async () => ({
      path: "/Users/alex/Desktop/Tidebreak conversations 2026-09-23.zip",
      bytes: 1_048_576,
      count: 4,
    }),
    deleteAllData: () => new Promise<boolean>(() => {}),
  };
}

/**
 * Settings → Data and privacy. The folder, its disk use, backup and export,
 * everything that leaves this computer, and the two actions that cannot be
 * undone.
 */
const meta = {
  title: "Settings/Data and privacy",
  component: DataPrivacyPanel,
  parameters: { layout: "fullscreen" },
  args: {
    client: stubClient() as never,
    host: stubHost(),
    attachedRemotely: false,
    conversations,
    onOpenSection: () => {},
    onReload: () => {},
    userAgent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)",
  },
} satisfies Meta<typeof DataPrivacyPanel>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The desktop, on its own data. */
export const Overview: Story = {};

/** The folder is still being measured. */
export const Loading: Story = {
  args: { client: stubClient({ pending: true }) as never },
};

/** The overview could not be read; retry is the one action. */
export const LoadFailed: Story = {
  args: { client: stubClient({ fail: true }) as never },
};

/** A deep data folder, which wraps rather than pushing the button away. */
export const LongPath: Story = {
  args: {
    client: stubClient({
      overview: {
        ...overview,
        data_dir:
          "/Users/alexandra.montgomery-whitfield/Library/Application Support/io.brightwave.tidebreak.staging/profiles/secondary-workspace",
      },
    }) as never,
  },
};

/**
 * A browser on a self-hosted server: no folder to open, nothing to delete
 * from this computer, and a backup its operator takes with PostgreSQL's own
 * tools.
 */
export const PostgresServer: Story = {
  args: {
    host: stubHost({ local: false }),
    client: stubClient({
      overview: {
        ...overview,
        data_dir: "/var/lib/tidebreak",
        storage: "postgres",
        usage: overview.usage.map((entry) =>
          entry.category === "database" ? { ...entry, bytes: 0 } : entry,
        ),
        backup_unavailable:
          "This server keeps conversations in PostgreSQL. Back it up with pg_dump, as the self-hosting guide describes.",
      },
    }) as never,
  },
};

/**
 * A member of a shared server in a browser. The folder, the backup, and the
 * settings are the administrator's; the member exports their own
 * conversations.
 */
export const SharedServerMember: Story = {
  args: {
    host: stubHost({ local: false }),
    client: stubClient({ member: true }) as never,
  },
};

/** This window works on another machine, so nothing here acts on this one. */
export const AttachedElsewhere: Story = {
  args: {
    attachedRemotely: true,
    client: stubClient({
      overview: { ...overview, data_dir: "/home/tidebreak/.tidebreak" },
    }) as never,
  },
};

/** A backup is streaming to the file the reader picked. */
export const BackingUp: Story = {
  args: { host: stubHost({ backupPending: true }) },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByRole("button", { name: "Back up…" }),
    );
    await canvas.findByRole("button", { name: "Backing up…" });
  },
};

/** Choosing which conversations to export, and the format. */
export const ExportChosen: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByRole("button", { name: "Export…" }),
    );
    const dialog = within(
      await within(canvasElement.ownerDocument.body).findByRole("dialog"),
    );
    await userEvent.click(dialog.getByRole("radio", { name: "Choose" }));
    await userEvent.click(
      dialog.getByRole("checkbox", {
        name: "Launch plan for the October release",
      }),
    );
    await userEvent.click(
      dialog.getByRole("checkbox", { name: "Quarterly revenue by region" }),
    );
  },
};

/** The reset confirmation lists exactly what resets and what stays. */
export const ResetConfirmation: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByRole("button", { name: "Reset settings…" }),
    );
    await within(canvasElement.ownerDocument.body).findByRole("alertdialog");
  },
};

/** Delete all data asks for the phrase before its button enables. */
export const DeleteConfirmation: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByRole("button", { name: "Delete all data…" }),
    );
    await within(canvasElement.ownerDocument.body).findByRole("alertdialog");
  },
};

/** The phrase is typed, so the destructive button is live. */
export const DeleteConfirmationTyped: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByRole("button", { name: "Delete all data…" }),
    );
    const dialog = within(
      await within(canvasElement.ownerDocument.body).findByRole("alertdialog"),
    );
    await userEvent.type(
      dialog.getByLabelText("Type delete all data to confirm."),
      "delete all data",
    );
  },
};
