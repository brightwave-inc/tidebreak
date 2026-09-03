import type { ReactNode } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";
import type { ApiClient, GatewayStatus, ManagedPolicy } from "@/api";
import { ErrorBoundary } from "@/ErrorBoundary";
import { ManagedGate } from "@/ManagedGate";
import { NetworkPolicyDialog } from "@/NetworkPolicyDialog";
import { gatewaySignedIn, gatewaySignedOut } from "./fixtures";

const unmanaged: ManagedPolicy = {
  managed: false,
  source: "unmanaged",
  misconfigured: false,
  allow_local_mcp_servers: false,
};

const managed: ManagedPolicy = {
  managed: true,
  source: "provisioned",
  misconfigured: false,
  allow_local_mcp_servers: false,
  gateway_url: "https://gateway.example.com",
};

function pending<T>(): Promise<T> {
  return new Promise(() => undefined);
}

function gateClient({
  policy = managed,
  status = gatewaySignedOut,
  policyLoad = "ready",
}: {
  policy?: ManagedPolicy;
  status?: GatewayStatus;
  policyLoad?: "ready" | "loading" | "failed";
} = {}): ApiClient {
  return {
    getPolicy: async () => {
      if (policyLoad === "loading") return pending();
      if (policyLoad === "failed") {
        throw new Error("503: managed policy could not be read");
      }
      return policy;
    },
    getGatewayStatus: async () => status,
    gatewaySignIn: async () => ({
      authorization_url: "https://gateway.example.com/sign-in",
    }),
    dismissGatewayPairing: async () => unmanaged,
  } as unknown as ApiClient;
}

function ProductReady({ children }: { children?: ReactNode }) {
  return (
    <div className="grid h-screen place-items-center bg-page-background p-8">
      <div className="max-w-lg rounded-xl border border-border-subtle bg-background p-8 text-center">
        <p className="text-xs font-medium tracking-wide text-muted-foreground uppercase">
          Managed session ready
        </p>
        <h1 className="mt-2 text-2xl font-semibold tracking-tight">
          Tidebreak is available
        </h1>
        <p className="mt-2 text-sm text-muted-foreground">
          {children ??
            "The shell renders after the gateway session matches policy."}
        </p>
      </div>
    </div>
  );
}

function ThrowsOnRender(): never {
  throw new Error("The conversation view could not render this result.");
}

const meta = {
  title: "Shell/Managed gate and recovery",
  component: ManagedGate,
  parameters: { layout: "fullscreen" },
  args: {
    client: gateClient(),
    children: <ProductReady />,
  },
} satisfies Meta<typeof ManagedGate>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Starting: Story = {
  args: { client: gateClient({ policyLoad: "loading" }) },
};

export const SignInRequired: Story = {};

export const SignInPending: Story = {
  args: {
    client: gateClient({
      status: {
        ...gatewaySignedOut,
        sign_in: {
          state: "pending",
          authorization_url: "https://gateway.example.com/sign-in/continue",
        },
      },
    }),
  },
};

export const SignInFailure: Story = {
  args: {
    client: gateClient({
      status: {
        ...gatewaySignedOut,
        sign_in: {
          state: "failed",
          message: "Your organization did not grant access to this gateway.",
        },
      },
    }),
  },
};

export const PairingRequested: Story = {
  args: {
    client: gateClient({
      policy: {
        ...unmanaged,
        pending_gateway_url: "https://gateway.new-company.example.com",
      },
    }),
  },
};

export const ManagedPolicyUnavailable: Story = {
  args: { client: gateClient({ policyLoad: "failed" }) },
};

export const ManagedPolicyMisconfigured: Story = {
  args: {
    client: gateClient({
      policy: { ...managed, gateway_url: undefined, misconfigured: true },
    }),
  },
};

export const ManagedSessionReady: Story = {
  args: { client: gateClient({ status: gatewaySignedIn }) },
};

export const UnexpectedShellError: Story = {
  render: () => (
    <ErrorBoundary onReload={fn()}>
      <ThrowsOnRender />
    </ErrorBoundary>
  ),
};

export const ContainedViewError: Story = {
  render: () => (
    <div className="mx-auto mt-10 max-w-xl rounded-xl border bg-background p-6">
      <h1 className="text-lg font-semibold">Conversation</h1>
      <p className="mt-2 text-sm text-muted-foreground">
        The rest of the view stays available when one result fails.
      </p>
      <ErrorBoundary
        fallback={
          <p className="mt-4 rounded-md bg-critical-background px-3 py-2 text-sm text-critical-foreground-muted">
            This result could not be shown.
          </p>
        }
      >
        <ThrowsOnRender />
      </ErrorBoundary>
    </div>
  ),
};

export const CustomNetworkPolicy: Story = {
  render: () => (
    <NetworkPolicyDialog
      open
      value={{
        mode: "allowed_hosts",
        allowed_hosts: ["api.github.com", "objects.githubusercontent.com"],
        package_managers: true,
      }}
      onOpenChange={fn()}
      onChange={fn(async () => {})}
    />
  ),
};

export const ManagedNetworkPolicy: Story = {
  render: () => (
    <NetworkPolicyDialog
      open
      disabled
      value={{ mode: "package_managers" }}
      onOpenChange={fn()}
      onChange={fn(async () => {})}
    />
  ),
};

export const NetworkPolicyFailure: Story = {
  render: () => (
    <NetworkPolicyDialog
      open
      value={{ mode: "open" }}
      onOpenChange={fn()}
      onChange={fn(async () => {
        throw new Error("This machine rejected the network policy update.");
      })}
    />
  ),
};
