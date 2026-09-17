import type { Meta, StoryObj } from "@storybook/react-vite";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import { expect, within } from "storybook/test";
import type { CodeSessionSnapshot } from "@/api/types";
import { CodeSessionTree } from "@/code/workspace/CodeSessionTree";

type Child = NonNullable<CodeSessionSnapshot["children"]>[number];

const running: Child = {
  id: "child-1",
  title: "Inspect the parser",
  status: "running",
  attention: false,
  fenced: false,
  workspace_id: "ws-parser",
  execution_location: "machine",
};
const done: Child = {
  id: "child-2",
  title: "Write the tests",
  status: "completed",
  attention: false,
  fenced: false,
  execution_location: "sandbox",
};
const paused: Child = {
  id: "child-3",
  title: "Review the fence",
  status: "fenced",
  attention: true,
  fenced: true,
  execution_location: "machine",
};
const failed: Child = {
  id: "child-4",
  title: "Apply the patch",
  status: "failed",
  attention: true,
  fenced: false,
  execution_location: "sandbox",
};

function TreeStory(props: {
  nodes: Child[];
  wait: CodeSessionSnapshot["wait"];
}) {
  const rootRoute = createRootRoute();
  const route = createRoute({
    getParentRoute: () => rootRoute,
    path: "/",
    component: () => (
      <div className="bg-background p-4">
        <CodeSessionTree nodes={props.nodes} wait={props.wait} />
      </div>
    ),
  });
  const sessionRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/code/s/$sessionId",
    component: () => <p>Opened child</p>,
  });
  const workspaceRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/code/w/$workspaceId",
    component: () => <p>Opened workspace</p>,
  });
  const router = createRouter({
    history: createMemoryHistory({ initialEntries: ["/"] }),
    routeTree: rootRoute.addChildren([route, sessionRoute, workspaceRoute]),
  });
  return <RouterProvider router={router} />;
}

const meta = {
  title: "Code/Session tree",
  component: TreeStory,
  parameters: { layout: "fullscreen" },
} satisfies Meta<typeof TreeStory>;
export default meta;
type Story = StoryObj<typeof meta>;

export const Waiting: Story = {
  args: {
    nodes: [
      running,
      done,
      paused,
      failed,
      { ...running, id: "child-5", title: "Search docs" },
    ],
    wait: { waiting: 3, total: 5 },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText("Waiting on 3 of 5")).toBeVisible();
    await expect(
      canvas.getByRole("button", { name: "Open Inspect the parser" }),
    ).toBeVisible();
  },
};

export const PartlySettled: Story = {
  args: {
    nodes: [done, running, failed],
    wait: { waiting: 1, total: 3 },
  },
};

export const PausedChild: Story = {
  args: {
    nodes: [paused],
    wait: { waiting: 1, total: 1 },
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.getByText("Needs attention")).toBeVisible();
    await expect(canvas.queryByText(/fenced/i)).toBeNull();
  },
};

export const DeepNarrow: Story = {
  args: {
    nodes: Array.from({ length: 6 }, (_, index) => ({
      ...running,
      id: `child-deep-${index}`,
      title: `Layer ${index + 1} of a nested investigation into the session tree`,
    })),
    wait: { waiting: 6, total: 6 },
  },
  decorators: [
    (Story) => (
      <div className="w-72 max-w-full">
        <Story />
      </div>
    ),
  ],
};

export const Empty: Story = {
  args: { nodes: [], wait: null },
};

export const ErrorChild: Story = {
  args: {
    nodes: [failed],
    wait: null,
  },
};

export const LongTitle: Story = {
  args: {
    nodes: [
      {
        ...running,
        title:
          "Compare every child conversation across repositories and report which ones still need a person",
      },
    ],
    wait: { waiting: 1, total: 1 },
  },
};
