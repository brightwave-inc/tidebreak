import { useEffect, useState } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import { fn, userEvent, within } from "storybook/test";
import type { Chat } from "@/api";
import { ChatHeaderTitle } from "@/ChatHeaderTitle";
import { useChatListStore } from "@/ChatListStore";
import { useConfirm } from "@/components/ConfirmDialog";
import { deletionDescription } from "@/ChatDeletion";
import { NewProjectDialog } from "@/sidebar/NewProjectDialog";
import { SidebarExpandStrip } from "@/sidebar/SidebarExpandStrip";
import { useActiveChatId } from "@/useActiveChatId";
import { WorkArchivePage } from "@/WorkArchivePage";
import { WorkLayout } from "@/WorkLayout";
import {
  activityAt,
  denseInboxEntries,
  denseRouteChats,
  groupedRouteChats,
  longTitleRouteChats,
  resetRouteStoryStores,
  routeChat,
  routeChats,
  RouteStoryProviders,
  storyClient,
} from "./routeStoryHarness";

type NavigationScenario =
  | "active-work"
  | "loading"
  | "empty"
  | "dense"
  | "failure"
  | "narrow"
  | "collapsed"
  | "collapsed-mac"
  | "grouped"
  | "running"
  | "unread"
  | "pinned"
  | "long-titles"
  | "archive"
  | "archive-empty"
  | "older-server"
  | "older-server-archive";

function NavigationSurface() {
  const activeChatId = useActiveChatId();
  const activeChat = useChatListStore(
    (state) =>
      state.chats.find((chat) => chat.id === activeChatId) ??
      state.archivedChats.find((chat) => chat.id === activeChatId),
  );
  return (
    <div className="content-container flex h-full min-h-0 flex-col">
      {activeChat && (
        <header
          className="window-chrome-row flex h-12 shrink-0 items-center border-b border-border-subtle pr-3"
          data-testid="work-window-chrome"
        >
          <ChatHeaderTitle chat={activeChat} />
        </header>
      )}
      <div className="grid min-h-0 flex-1 place-items-center p-8">
        <div className="max-w-md text-center">
          <h1 className="text-2xl font-semibold tracking-tight">
            Navigation review surface
          </h1>
          <p className="mt-2 text-sm text-muted-foreground">
            This canvas keeps the production rail in context while you inspect
            active routes, list density, loading, and failure states.
          </p>
        </div>
      </div>
    </div>
  );
}

/**
 * The same shape as the app's router: one pathless Work layout that mounts
 * the rail once, with every Work route as its child.
 */
function createNavigationRouter(initialPath: string) {
  const rootRoute = createRootRoute();
  const layoutRoute = createRoute({
    getParentRoute: () => rootRoute,
    id: "work-layout",
    component: WorkLayout,
  });
  const surface = (path: string) =>
    createRoute({
      getParentRoute: () => layoutRoute,
      path,
      component: NavigationSurface,
    });
  const archiveRoute = createRoute({
    getParentRoute: () => layoutRoute,
    path: "/archive",
    component: WorkArchivePage,
  });

  return createRouter({
    routeTree: rootRoute.addChildren([
      layoutRoute.addChildren([
        surface("/"),
        surface("/inbox"),
        surface("/apps"),
        surface("/plugins"),
        surface("/settings"),
        surface("/code"),
        surface("/p/$projectId"),
        surface("/c/$chatId"),
        surface("/p/$projectId/c/$chatId"),
        archiveRoute,
      ]),
    ]),
    history: createMemoryHistory({ initialEntries: [initialPath] }),
  });
}

/** Two turns in flight, one of them in the conversation on screen. */
const runningChats: Chat[] = [
  routeChat({
    id: "running-1",
    title: "Draft the launch announcement",
    last_activity_at: activityAt(0, 1),
    running: true,
  }),
  routeChat({
    id: "running-2",
    title: "Build the churn analysis workbook",
    last_activity_at: activityAt(0, 4),
    running: true,
  }),
  routeChat({
    id: "running-3",
    title: "Plan the offsite agenda",
    last_activity_at: activityAt(0, 40),
  }),
];

/** Turns that finished while the reader was in another conversation. */
const unreadChats: Chat[] = [
  routeChat({
    id: "unread-1",
    title: "Summarize the pricing survey",
    last_activity_at: activityAt(0, 6),
    unread: true,
  }),
  routeChat({
    id: "unread-2",
    title: "Reconcile the September invoices",
    last_activity_at: activityAt(0, 25),
    unread: true,
  }),
  routeChat({
    id: "unread-3",
    title: "Compare vendor security reviews",
    last_activity_at: activityAt(0, 55),
  }),
];

const pinnedChats: Chat[] = [
  routeChat({
    id: "pin-1",
    title: "Quarterly board deck",
    pinned_at: activityAt(0, 10),
    last_activity_at: activityAt(6),
  }),
  routeChat({
    id: "pin-2",
    title: "Customer interview synthesis",
    pinned_at: activityAt(3),
    last_activity_at: activityAt(1),
    unread: true,
  }),
  routeChat({
    id: "pin-3",
    title: "Hiring plan",
    pinned_at: activityAt(8),
    last_activity_at: activityAt(20),
  }),
  routeChat({
    id: "pin-4",
    title: "Draft the launch announcement",
    last_activity_at: activityAt(0, 3),
  }),
];

const LIST_FIELDS = new Set([
  "last_activity_at",
  "pinned_at",
  "archived_at",
  "running",
  "unread",
  "turn_count",
]);

/**
 * A conversation as a server older than the list of work sends it: the bare
 * conversation, without pins, the archive, unread marks, or turn counts.
 */
function fromOlderServer(chat: Chat): Chat {
  return Object.fromEntries(
    Object.entries(chat).filter(([key]) => !LIST_FIELDS.has(key)),
  ) as Chat;
}

const olderServerChats = groupedRouteChats.slice(0, 6).map(fromOlderServer);

const scenarioChats: Partial<Record<NavigationScenario, Chat[]>> = {
  grouped: groupedRouteChats,
  running: runningChats,
  unread: unreadChats,
  pinned: pinnedChats,
  "long-titles": longTitleRouteChats,
  archive: groupedRouteChats.slice(0, 6),
  "archive-empty": groupedRouteChats.slice(0, 6),
  "older-server": olderServerChats,
  "older-server-archive": olderServerChats,
};

const scenarioPaths: Partial<Record<NavigationScenario, string>> = {
  "active-work": "/c/chat-1",
  dense: "/inbox",
  narrow: "/apps",
  collapsed: "/apps",
  "collapsed-mac": "/c/chat-1",
  grouped: "/c/today-3",
  running: "/c/running-1",
  unread: "/c/unread-3",
  pinned: "/c/pin-4",
  "long-titles": "/c/long-2",
  archive: "/archive",
  "archive-empty": "/archive",
  "older-server": `/c/${olderServerChats[1]?.id}`,
  "older-server-archive": "/archive",
};

function NavigationStory({ scenario }: { scenario: NavigationScenario }) {
  const [state] = useState(() => {
    const dense = scenario === "dense";
    const chats =
      scenarioChats[scenario] ??
      (scenario === "empty" || scenario === "loading"
        ? []
        : dense
          ? denseRouteChats
          : routeChats);
    resetRouteStoryStores({
      chats,
      chatsLoaded: scenario !== "loading",
      chatsError:
        scenario === "failure"
          ? "Work could not be loaded from this machine."
          : null,
      projects:
        scenario === "empty" ||
        scenario === "loading" ||
        scenario in scenarioChats
          ? []
          : undefined,
      projectsLoaded: scenario !== "loading",
      inboxEntries: dense ? denseInboxEntries : [],
      inboxLoaded: scenario !== "loading",
      attentionChatIds:
        scenario === "grouped"
          ? ["yesterday-2"]
          : dense
            ? ["chat-2", "dense-chat-2"]
            : ["chat-2"],
      sidebarWidth: scenario === "narrow" ? 220 : 280,
      sidebarCollapsed:
        scenario === "collapsed" || scenario === "collapsed-mac",
    });

    return {
      client: storyClient(
        scenario === "archive-empty"
          ? { listChats: async () => [] }
          : scenario === "older-server-archive"
            ? // An older server ignores the archive question and answers
              // with every conversation.
              { listChats: async () => olderServerChats }
            : {},
      ),
      router: createNavigationRouter(scenarioPaths[scenario] ?? "/"),
    };
  });

  return (
    <RouteStoryProviders client={state.client}>
      <div className="app-shell h-full min-h-0 w-full overflow-hidden">
        <SidebarExpandStrip macOverlay={scenario === "collapsed-mac"} />
        <div className="app-body">
          <RouterProvider router={state.router as never} />
        </div>
      </div>
    </RouteStoryProviders>
  );
}

const meta = {
  title: "Navigation/Sidebar",
  component: NavigationStory,
  args: { scenario: "active-work" },
  parameters: { layout: "fullscreen" },
  render: (args) => <NavigationStory key={args.scenario} {...args} />,
} satisfies Meta<typeof NavigationStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const ActiveWork: Story = {};

export const LoadingLists: Story = {
  args: { scenario: "loading" },
};

export const EmptyLists: Story = {
  args: { scenario: "empty" },
};

export const DenseLists: Story = {
  args: { scenario: "dense" },
};

export const ChatLoadFailure: Story = {
  args: { scenario: "failure" },
};

export const NarrowRail: Story = {
  args: { scenario: "narrow" },
  parameters: {
    docs: {
      description: {
        story:
          "The rail stays fully featured at its narrower saved width. This replaces the misleading compact-rail name; compact now refers only to viewport stories.",
      },
    },
  },
};

export const CollapsedRail: Story = {
  args: { scenario: "collapsed" },
};

/** At 720px the rail leaves the layout for the expand strip. */
export const MinimumWindow: Story = {
  globals: { viewport: { value: "minimumWindow", isRotated: false } },
};

/** Above the overlay breakpoint the remembered rail width returns. */
export const DesktopWidth: Story = {
  globals: { viewport: { value: "desktop", isRotated: false } },
};

export const CollapsedMacWork: Story = {
  args: { scenario: "collapsed-mac" },
  globals: { viewport: { value: "minimumWindow", isRotated: false } },
};

export const CollapseAndRestoreActiveRoute: Story = {
  args: { scenario: "narrow" },
};

/**
 * The list of work by date: Pinned, Today, Yesterday, Previous 7 days, and
 * Older. The untitled conversation nothing happened in is left out.
 */
export const GroupedList: Story = {
  args: { scenario: "grouped" },
};

/** A turn in flight carries the live mark, the open conversation included. */
export const RunningWork: Story = {
  args: { scenario: "running" },
};

/** A turn that finished while you were in another conversation. */
export const UnreadWork: Story = {
  args: { scenario: "unread" },
};

/** Pinned conversations sit in their own group at the top. */
export const PinnedWork: Story = {
  args: { scenario: "pinned" },
};

/** Long titles truncate on one line and keep their status mark. */
export const LongTitles: Story = {
  args: { scenario: "long-titles" },
};

/** The archive, reached from the Work list's options. */
export const ArchivedView: Story = {
  args: { scenario: "archive" },
};

export const EmptyArchive: Story = {
  args: { scenario: "archive-empty" },
};

/**
 * Attached to a server older than the list of work: every row stays, grouped
 * by creation, and the row menu leaves out pin and archive.
 */
export const OlderServerRowMenu: Story = {
  args: { scenario: "older-server" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByRole("button", {
        name: `Actions for ${olderServerChats[0]?.title}`,
      }),
    );
  },
};

/** The archive page, reached by a link, on a server that predates it. */
export const OlderServerArchive: Story = {
  args: { scenario: "older-server-archive" },
};

/** A project's row menu, where Instructions opens the project's brief. */
export const ProjectMenu: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByRole("button", {
        name: "Actions for Desktop release",
      }),
    );
  },
};

/** A row's menu: pin and archive beside rename, move, and delete. */
export const WorkRowMenu: Story = {
  args: { scenario: "grouped" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByRole("button", {
        name: "Actions for Summarize the pricing survey",
      }),
    );
  },
};

function DeleteConfirmationStory({
  outputs,
  folders,
}: {
  outputs: number | null;
  folders: number;
}) {
  const { decide, dialog } = useConfirm();
  useEffect(() => {
    void decide({
      title: "Delete Quarterly board deck?",
      description: deletionDescription({ folders, outputs }),
      alternativeLabel: "Archive",
      confirmLabel: "Delete work",
      destructive: true,
    });
  }, [decide, folders, outputs]);
  return dialog;
}

/** Deleting says which outputs go with the conversation, and offers Archive. */
export const DeleteConfirmation: Story = {
  render: () => <DeleteConfirmationStory outputs={3} folders={0} />,
};

export const DeleteConfirmationWithFolders: Story = {
  render: () => <DeleteConfirmationStory outputs={1} folders={2} />,
};

export const NewProject: Story = {
  render: () => (
    <NewProjectDialog
      open
      creating={false}
      onOpenChange={fn()}
      onCreate={fn(async () => true)}
    />
  ),
};

export const CreatingProject: Story = {
  render: () => (
    <NewProjectDialog
      open
      creating
      onOpenChange={fn()}
      onCreate={fn(async () => true)}
    />
  ),
};
