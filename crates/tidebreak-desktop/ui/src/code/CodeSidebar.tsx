import { useEffect } from "react";
import { useNavigate, useRouterState } from "@tanstack/react-router";
import { Archive, Bell, GitPullRequest, Plus } from "lucide-react";
import { toast } from "sonner";

import { useApp } from "@/AppContext";
import { cn, friendlyErrorMessage } from "@/lib/utils";
import { useLayoutState } from "@/panel/usePanelNav";
import { SidebarFrame } from "@/sidebar/SidebarFrame";
import { SidebarButton } from "@/sidebar/primitives";
import { AddRepoPalette } from "./AddRepoPalette";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { CodeModeSwitch } from "./CodeModeSwitch";
import {
  unreadCodeDeliveryNotifications,
  useCodeDeliveryStore,
} from "./CodeDeliveryStore";
import { CodeSubscriptionUsage } from "./CodeSubscriptionUsage";
import { useCodeUiStore } from "./CodeUiStore";
import {
  connectCodeUpdates,
  useCodeUpdatesStore,
  useWorkspaceDigests,
  watchChildren,
} from "./CodeUpdatesStore";
import { FOCUS_RING, HOVER_TINT } from "./interactive";
import { NewWorkspaceDialog } from "./NewWorkspaceDialog";
import { RailSettingsMenu } from "./RailSettingsMenu";
import { RepoSwitcherPopover } from "./RepoSwitcherPopover";
import {
  useWorkspaceCardCommands,
  workspaceCommands,
} from "./workspaceActions";
import { WorkspaceCard } from "./WorkspaceCard";
import { arrangeWorkspaces } from "./workspaceCards";
import { prWorkflowPrompt } from "./prActions";
import type { WorkspaceWorkflowAction } from "./workspaceWorkflow";

/**
 * The code-mode rail: one toolbar, then workspace cards.
 *
 * Built on the same frame and primitives as the chat rail so the two modes
 * share chrome. It must not touch chat session stores — that separation is
 * what later convergence merges rather than translates.
 *
 * The rail spends no section on the repo catalog: by-repo group headers link
 * to their repo page, and the toolbar's repo switcher covers the other sort
 * orders. What each card draws is the reader's `railPrefs` choice; what each
 * card *says* (the aria-label) never shrinks with those choices.
 *
 * It also hosts code mode's two dialogs. The rail is on screen for every
 * `/code` route, so mounting them here is what makes them reachable from the
 * keyboard anywhere in the mode without a second instance per page.
 */

export function CodeSidebar() {
  const navigate = useNavigate();
  const { client } = useApp();
  const pathname = useRouterState({
    select: (state) => state.location.pathname,
  });
  const repos = useCodeCatalogStore((state) => state.repos);
  const workspaces = useCodeCatalogStore((state) => state.workspaces);
  const sessions = useCodeCatalogStore((state) => state.sessionsByWorkspace);
  const refresh = useCodeCatalogStore((state) => state.refresh);
  const digests = useWorkspaceDigests();
  const childrenByWorkspace = useCodeUpdatesStore(
    (state) => state.childrenByWorkspace,
  );
  const newWorkspaceOpen = useCodeUiStore((state) => state.newWorkspaceOpen);
  const newWorkspaceRepoId = useCodeUiStore(
    (state) => state.newWorkspaceRepoId,
  );
  const addRepoOpen = useCodeUiStore((state) => state.addRepoOpen);
  const startNewWorkspace = useCodeUiStore((state) => state.startNewWorkspace);
  const setNewWorkspaceOpen = useCodeUiStore(
    (state) => state.setNewWorkspaceOpen,
  );
  const setAddRepoOpen = useCodeUiStore((state) => state.setAddRepoOpen);
  const prefs = useCodeUiStore((state) => state.railPrefs);
  const runComposerPrompt = useCodeUiStore((state) => state.runComposerPrompt);
  const unreadNotifications = useCodeDeliveryStore((state) =>
    unreadCodeDeliveryNotifications(state),
  );
  const { run, dialogs } = useWorkspaceCardCommands();
  const layout = useLayoutState();
  const terminalOpen =
    layout.tabs.some((tab) => tab.type === "terminal") &&
    pathname.startsWith("/code/w/");
  const viewedWorkspaceId = pathname.startsWith("/code/w/")
    ? pathname.slice("/code/w/".length).split("/")[0]
    : undefined;

  useEffect(() => {
    void refresh(client);
  }, [client, refresh]);

  useEffect(() => connectCodeUpdates(client), [client]);

  const groups = arrangeWorkspaces(
    prefs.sortMode,
    repos,
    workspaces,
    digests,
  );

  return (
    <SidebarFrame
      footer={
        <>
          <CodeUtilityLinks
            pathname={pathname}
            unreadNotifications={unreadNotifications}
            onNavigate={(to) => void navigate({ to })}
          />
          <div className="mt-1 border-t border-border-subtle pt-1">
            <CodeSubscriptionUsage />
          </div>
        </>
      }
    >
      <CodeModeSwitch />

      <div className="px-2 pt-1">
        <button
          type="button"
          className={cn(
            "flex h-9 w-full cursor-pointer items-center gap-2 rounded-xl border border-border-subtle bg-background px-2.5 text-left text-[13px] font-medium shadow-[0_1px_2px_color-mix(in_oklch,var(--foreground)_5%,transparent)] hover:bg-muted/55",
            FOCUS_RING,
            HOVER_TINT,
          )}
          onClick={() => startNewWorkspace()}
        >
          <Plus size={14} />
          <span>New workspace</span>
        </button>
      </div>

      <div className="flex items-center gap-0.5 px-2 pt-1.5 pb-1.5">
        <RepoSwitcherPopover repos={repos} />
        <div className="flex-1" />
        <RailSettingsMenu />
      </div>

      <div className="flex min-h-0 flex-col gap-1">
        {groups.map((group) => (
          <div key={group.key} className="flex flex-col gap-1">
            {group.label && group.repoId ? (
              <GroupHeaderLink
                label={group.label}
                active={pathname === `/code/r/${group.repoId}`}
                onOpen={() =>
                  void navigate({
                    to: "/code/r/$repoId",
                    params: { repoId: group.repoId! },
                  })
                }
              />
            ) : (
              group.label && (
                <div
                  className="truncate px-2 pt-3 pb-1 text-[11px] font-medium text-muted-foreground/90"
                  title={group.label}
                >
                  {group.key === "archived"
                    ? `${group.label} · ${group.workspaces.length}`
                    : group.label}
                </div>
              )
            )}
            {group.workspaces.map((workspace) => {
              const digest = digests[workspace.id];
              const pr = digest?.pr_state ?? workspace.pr;
              return (
                <WorkspaceCard
                  key={workspace.id}
                  workspace={workspace}
                  digest={digest}
                  session={sessions[workspace.id]}
                  repoName={
                    repos.find((repo) => repo.id === workspace.repo_id)
                      ?.display_name ?? workspace.repo_id
                  }
                  active={pathname === `/code/w/${workspace.id}`}
                  terminalOpen={terminalOpen && viewedWorkspaceId === workspace.id}
                  density={prefs.density}
                  visibleMeta={{
                    // The group header already names the repo in by-repo
                    // order; the chip would say it twice on every row.
                    repoChip:
                      prefs.showRepoChip && prefs.sortMode !== "by-repo",
                    branch: prefs.showBranch,
                  }}
                  commands={workspaceCommands({
                    hasPr: Boolean(pr),
                    archived: workspace.status === "archived",
                    hasSession: Boolean(sessions[workspace.id]),
                    attentionPinned:
                      (digest?.attention ?? sessions[workspace.id]?.attention)
                        ?.state.type === "manual",
                  })}
                  childSessions={watchChildren(
                    { childrenByWorkspace },
                    workspace.id,
                  )}
                  onOpen={() =>
                    void navigate({
                      to: "/code/w/$workspaceId",
                      params: { workspaceId: workspace.id },
                    })
                  }
                  onOpenChildSession={(sessionId) =>
                    void navigate({
                      to: "/code/w/$workspaceId",
                      params: { workspaceId: workspace.id },
                      search: { task: sessionId },
                    })
                  }
                  onOpenSubagent={(callId) =>
                    void navigate({
                      to: "/code/w/$workspaceId",
                      params: { workspaceId: workspace.id },
                      search: { subagent: callId },
                    })
                  }
                  onWorkflowAction={(action: WorkspaceWorkflowAction) => {
                    if (action === "open_pr") {
                      run("open-pr", {
                        workspace,
                        title: digest?.title ?? workspace.title,
                        pr,
                        session: sessions[workspace.id],
                      });
                      return;
                    }
                    if (action === "watch_and_fix") {
                      void client
                        .startCodeWatch(workspace.id)
                        .then(() => toast.success("Watching the pull request"))
                        .catch((error) =>
                          toast.error(
                            friendlyErrorMessage(
                              error,
                              "Could not start the watch",
                            ),
                          ),
                        );
                      return;
                    }
                    if (
                      action === "open_source" ||
                      action === "push" ||
                      action === "create_pr" ||
                      action === "merge"
                    ) {
                      // Local-git stages never arise from the digest-only
                      // model; the workspace page is where they resolve.
                      // Merging goes there too: decision 42 makes it the
                      // reader's call, and a card in a rail is the wrong place
                      // to land a shared branch from — the header puts the
                      // pull request in front of them first.
                      void navigate({
                        to: "/code/w/$workspaceId",
                        params: { workspaceId: workspace.id },
                      });
                      return;
                    }
                    if (!pr) return;
                    // Same prepared prompt the header control composes; the
                    // navigation makes the started turn visible.
                    if (!runComposerPrompt(workspace.id, prWorkflowPrompt(action, pr))) {
                      toast.error("Another agent action is already running");
                      return;
                    }
                    void navigate({
                      to: "/code/w/$workspaceId",
                      params: { workspaceId: workspace.id },
                    });
                  }}
                  onCommand={(command) =>
                    run(command, {
                      workspace,
                      title: digest?.title ?? workspace.title,
                      pr,
                      session: sessions[workspace.id],
                    })
                  }
                />
              );
            })}
          </div>
        ))}
        {repos.length === 0 && (
          <SidebarEmptyAction
            label="Add a repo"
            onClick={() => setAddRepoOpen(true)}
          />
        )}
        {repos.length > 0 &&
          groups.every((group) => group.workspaces.length === 0) && (
            <SidebarEmptyAction
              label="New workspace"
              onClick={() => startNewWorkspace()}
            />
          )}
      </div>

      {dialogs}
      <NewWorkspaceDialog
        open={newWorkspaceOpen}
        onOpenChange={setNewWorkspaceOpen}
        repos={repos}
        defaultRepoId={newWorkspaceRepoId}
      />
      <AddRepoPalette open={addRepoOpen} onOpenChange={setAddRepoOpen} />
    </SidebarFrame>
  );
}

function CodeUtilityLinks({
  pathname,
  unreadNotifications,
  onNavigate,
}: {
  pathname: string;
  unreadNotifications: number;
  onNavigate: (
    to:
      | "/code/delivery/pull-requests"
      | "/code/archive"
      | "/code/notifications",
  ) => void;
}) {
  const links = [
    {
      label: "Delivery",
      to: "/code/delivery/pull-requests" as const,
      active: pathname.startsWith("/code/delivery/"),
      icon: GitPullRequest,
    },
    {
      label: "Archive",
      to: "/code/archive" as const,
      active: pathname === "/code/archive",
      icon: Archive,
    },
    {
      label: "Notifications",
      to: "/code/notifications" as const,
      active: pathname === "/code/notifications",
      icon: Bell,
    },
  ];
  return (
    <div className="flex flex-col gap-0.5">
      {links.map((link) => (
        <SidebarButton
          key={link.to}
          type="button"
          aria-current={link.active ? "page" : undefined}
          data-active={link.active || undefined}
          className="data-[active]:bg-muted"
          onClick={() => onNavigate(link.to)}
        >
          <link.icon />
          <span className="min-w-0 flex-1 truncate">{link.label}</span>
          {link.to === "/code/notifications" && unreadNotifications > 0 && (
            <span className="min-w-5 rounded-full bg-primary px-1.5 py-0.5 text-center text-[10px] font-medium leading-none text-primary-foreground">
              {unreadNotifications > 99 ? "99+" : unreadNotifications}
            </span>
          )}
        </SidebarButton>
      ))}
    </div>
  );
}

/**
 * A by-repo group header that is also the way into its repo page.
 *
 * The old rail spent a whole section restating these names as buttons; the
 * header was already the label readers scanned, so the header is the control.
 */
function GroupHeaderLink({
  label,
  active,
  onOpen,
}: {
  label: string;
  active: boolean;
  onOpen: () => void;
}) {
  return (
    <button
      type="button"
      aria-label={`Open repo ${label}`}
      aria-current={active ? "page" : undefined}
      data-active={active || undefined}
      className={cn(
        "mt-3 cursor-pointer truncate rounded-lg px-2 py-1 text-left text-[11px] font-medium text-muted-foreground/90 hover:bg-background hover:text-foreground data-[active]:text-foreground",
        FOCUS_RING,
        HOVER_TINT,
      )}
      title={label}
      onClick={onOpen}
    >
      {label}
    </button>
  );
}

/**
 * A rail section with nothing in it yet.
 *
 * The empty slot is where the reader is already looking, and the section's own
 * `+` is a 14px target beside a heading. Making the line itself the control
 * answers "what now?" where the question is asked, and adds no chrome: it
 * replaces the label that was there.
 */
function SidebarEmptyAction({
  label,
  onClick,
}: {
  label: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      className={cn(
        "text-muted-foreground hover:bg-muted hover:text-foreground cursor-pointer rounded-md px-2 py-1 text-left text-xs",
        FOCUS_RING,
        HOVER_TINT,
      )}
      onClick={onClick}
    >
      {label}
    </button>
  );
}
