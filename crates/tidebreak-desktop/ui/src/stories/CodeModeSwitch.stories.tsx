import { useState } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";

import type { AppMode } from "@/appMode";
import { CodeModeSwitch } from "@/code/CodeModeSwitch";

function ModeSwitchPreview({ mode }: { mode: AppMode }) {
  const [router] = useState(() => {
    const rootRoute = createRootRoute();
    const routes = ["/code", "/"].map((path) =>
      createRoute({
        getParentRoute: () => rootRoute,
        path,
        component: CodeModeSwitch,
      }),
    );
    return createRouter({
      routeTree: rootRoute.addChildren(routes),
      history: createMemoryHistory({
        initialEntries: [mode === "code" ? "/code" : "/"],
      }),
    });
  });

  return (
    <div className="w-64">
      <RouterProvider router={router as never} />
    </div>
  );
}

const meta = {
  title: "Modes/Switch",
  component: ModeSwitchPreview,
  args: { mode: "code" },
  render: (args) => <ModeSwitchPreview key={args.mode} {...args} />,
} satisfies Meta<typeof ModeSwitchPreview>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Code is the first-run default and the leftmost segment. */
export const CodeSelected: Story = {};

/** A saved Work preference still selects the right segment. */
export const WorkSelected: Story = {
  args: { mode: "work" },
};
