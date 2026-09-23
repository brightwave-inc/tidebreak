import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn, userEvent, within } from "storybook/test";

import type { ApiClient, GatewayStatus, RemoteMachineState } from "@/api";
import type { GatewayLeaveControl } from "@/gatewayLeave";
import { GatewayPanel, type MachineControls } from "@/settings/GatewayPanel";
import {
  gatewayApps,
  gatewaySignedIn,
  gatewaySignedOut,
  machineAttached,
  machineLocal,
} from "./fixtures";

/**
 * A client that answers the panel's three reads and refuses to write.
 *
 * `offered` is the machine the gateway says it hosts. Absent is its own
 * state, not a failure: a gateway that hosts nothing and one older than the
 * field both answer that way, and the address field is simply empty.
 */
function stubClient(status: GatewayStatus, offered?: string): ApiClient {
  return {
    getGatewayStatus: async () => status,
    getGatewayApps: async () => gatewayApps,
    getGatewayMachine: async () => ({ url: offered }),
  } as unknown as ApiClient;
}

/**
 * The machine half, without a native shell. Attaching and detaching resolve
 * and go no further — a story that could reattach the window would take the
 * canvas with it.
 */
function stubMachine(
  state: RemoteMachineState,
  { detachable = true }: { detachable?: boolean } = {},
): MachineControls {
  return {
    read: async () => state,
    attachWithGateway: fn(async () => state),
    attachWithToken: fn(async () => state),
    detach: fn(async () => machineLocal),
    detachable,
    reattach: fn(),
  };
}

/**
 * Leaving without a native shell. The story resolves and stays on the
 * managed panel; in the app, the gate re-reads policy and the panel turns
 * into the unmanaged one.
 */
function stubLeave(available = true): GatewayLeaveControl {
  return { available, leave: fn(async () => undefined) };
}

/**
 * Model Gateway settings, which now also say which machine this window works
 * on. The states that matter are the two questions a reader arrives with: who
 * governs this profile, and where does my work run.
 */
const meta = {
  title: "Settings/Model Gateway",
  component: GatewayPanel,
  parameters: { layout: "fullscreen" },
  decorators: [
    (Story, context) => (
      <div
        className="h-full min-w-0"
        style={{
          width: context.globals.viewport === "compact" ? 420 : "100%",
          maxWidth: "100%",
        }}
      >
        <Story />
      </div>
    ),
  ],
  args: {
    client: stubClient(gatewaySignedIn),
    managed: true,
    source: "os",
    gatewayUrl: "https://gateway.example.com",
    onChanged: fn(),
    onOpenConnectedApps: fn(),
    machine: stubMachine(machineLocal),
    leave: stubLeave(),
  },
} satisfies Meta<typeof GatewayPanel>;

export default meta;
type Story = StoryObj<typeof meta>;

/**
 * No gateway at all. The section stays on the rail because the machine lives
 * here: a machine behind no gateway is reachable with its own token, under
 * Advanced, and this is the only place in the app that reaches it.
 */
export const Unmanaged: Story = {
  args: {
    client: stubClient(gatewaySignedOut),
    managed: false,
    source: "unmanaged",
    gatewayUrl: null,
  },
};

/** Managed by policy, signed in to nothing yet. */
export const SignedOut: Story = {
  args: { client: stubClient(gatewaySignedOut) },
};

/** The browser flow is still pending and can be restarted without waiting. */
export const PendingSignIn: Story = {
  args: {
    client: stubClient({
      ...gatewaySignedOut,
      sign_in: {
        state: "pending",
        authorization_url: "https://gateway.example.com/sign-in/pending",
      },
    }),
  },
};

/**
 * Signed in, with the entitled apps the deployment grants. The organization's
 * device policy put the gateway here, so the panel names it and offers no way
 * to leave: that is the administrator's to change.
 */
export const SignedIn: Story = {};

/**
 * The person connected this gateway through a link. The panel says so and
 * when, and the danger zone at the bottom lets them leave it.
 */
export const ConnectedThroughALink: Story = {
  args: {
    source: "provisioned",
    provisionedAt: "2026-09-21T15:30:00Z",
  },
};

/**
 * Leaving asks first. The confirmation names the gateway and what comes back:
 * the person's own provider keys and settings.
 */
export const LeaveConfirmation: Story = {
  args: {
    source: "provisioned",
    provisionedAt: "2026-09-21T15:30:00Z",
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByRole("button", { name: "Leave gateway" }),
    );
    await within(canvasElement.ownerDocument.body).findByRole("alertdialog");
  },
};

/**
 * The shell refused: the policy changed between the confirmation and the
 * delete. The reason shows beside the control the person used.
 */
export const LeaveRefused: Story = {
  args: {
    source: "provisioned",
    provisionedAt: "2026-09-21T15:30:00Z",
    leave: {
      available: true,
      leave: fn(async () => {
        throw "The gateway managing this device changed while disconnecting. Nothing was changed; try again from the new state.";
      }),
    },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByRole("button", { name: "Leave gateway" }),
    );
    const body = within(canvasElement.ownerDocument.body);
    await body.findByRole("alertdialog");
    const confirm = body.getAllByRole("button", { name: "Leave gateway" });
    await userEvent.click(confirm[confirm.length - 1]);
    await canvas.findByText(/changed while disconnecting/);
  },
};

/**
 * The gateway names the machine it hosts, so the address is already filled
 * in and connecting is one deliberate click. Nothing attaches on its own:
 * attaching moves your work to another machine and takes away connected
 * folders, computer use, and native export.
 */
export const MachineOffered: Story = {
  args: {
    client: stubClient(gatewaySignedIn, "https://tidebreak.example.com"),
  },
};

/**
 * Attached. The conversation and the agents run on the other machine, and
 * everything that reaches this computer is struck through.
 */
export const Attached: Story = {
  args: {
    client: stubClient(gatewaySignedIn, "https://tidebreak.example.com"),
    machine: stubMachine(machineAttached),
  },
};

/**
 * Attached to a gateway-authenticated hosted machine.
 *
 * The reader signed in to the gateway on their own computer and attached with
 * that account, so the machine holds their credential and none of its own. It
 * can never report a session, which is why the gateway is named read-only
 * with no sign-in, no sign-out, and no sync.
 */
export const HostedMachine: Story = {
  args: {
    client: stubClient(gatewaySignedOut),
    managed: false,
    source: "unmanaged",
    gatewayUrl: null,
    hostedGatewayUrl: "https://gateway.example.com/",
    machine: stubMachine(machineAttached),
  },
};

/**
 * The same hosted machine, opened in a browser tab the machine served.
 *
 * There is no server inside a browser tab to return to, so the disconnect
 * control is replaced by the one thing the reader can do: close the tab, or
 * attach from the desktop app.
 */
export const HostedMachineInBrowser: Story = {
  args: {
    client: stubClient(gatewaySignedOut),
    managed: false,
    source: "unmanaged",
    gatewayUrl: null,
    hostedGatewayUrl: "https://gateway.example.com/",
    machine: stubMachine(machineAttached, { detachable: false }),
  },
};
