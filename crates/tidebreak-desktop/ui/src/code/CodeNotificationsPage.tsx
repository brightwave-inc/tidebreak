import { useEffect, useMemo, useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { formatDistanceToNowStrict } from "date-fns";
import {
  Bell,
  CheckCheck,
  CircleAlert,
  ExternalLink,
  GitBranch,
  GitPullRequest,
  Settings2,
  Trash2,
  Workflow,
} from "lucide-react";

import { useApp } from "@/AppContext";
import { SearchInput } from "@/components/SearchInput";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { cn } from "@/lib/utils";
import { openInBrowser } from "@/openInBrowser";
import { RouteFrame } from "@/RouteFrame";
import type { CodeGitHubRepositoryRef } from "../api/types";
import {
  codeDeliveryRepositoryKey,
  trackedCodeDeliveryRepositories,
  unreadCodeDeliveryNotifications,
  useCodeDeliveryStore,
  type CodeDeliveryNotification,
  type CodeDeliveryNotificationRule,
  type CodeDeliveryNotificationRuleKind,
} from "./CodeDeliveryStore";
import { CodeSidebar } from "./CodeSidebar";

type FeedFilter = "all" | "unread" | CodeDeliveryNotificationRuleKind;

export function CodeNotificationsPage() {
  return (
    <RouteFrame sidebar={<CodeSidebar />}>
      <div className="content-container min-h-0 w-full min-w-0 flex-1 overflow-hidden">
        <CodeNotificationsBody />
      </div>
    </RouteFrame>
  );
}

function CodeNotificationsBody() {
  const { client } = useApp();
  const navigate = useNavigate();
  const notifications = useCodeDeliveryStore((state) => state.notifications);
  const rules = useCodeDeliveryStore((state) => state.notificationRules);
  const manualRepositories = useCodeDeliveryStore(
    (state) => state.manualRepositories,
  );
  const excludedRegisteredRepoIds = useCodeDeliveryStore(
    (state) => state.excludedRegisteredRepoIds,
  );
  const pinnedRepositoryKeys = useCodeDeliveryStore(
    (state) => state.pinnedRepositoryKeys,
  );
  const polling = useCodeDeliveryStore((state) => state.polling);
  const monitorError = useCodeDeliveryStore((state) => state.monitorError);
  const lastSuccessfulPollAt = useCodeDeliveryStore(
    (state) => state.lastSuccessfulPollAt,
  );
  const [tab, setTab] = useState("feed");
  const [search, setSearch] = useState("");
  const [filter, setFilter] = useState<FeedFilter>("all");
  const [discovered, setDiscovered] = useState<CodeGitHubRepositoryRef[]>([]);

  useEffect(() => {
    let cancelled = false;
    void client
      .getCodeDeliveryRepositories()
      .then((snapshot) => {
        if (!cancelled) setDiscovered(snapshot.repositories);
      })
      .catch(() => {
        // The monitor owns the visible error. Rules can still edit global scope.
      });
    return () => {
      cancelled = true;
    };
  }, [client]);

  const repositories = useMemo(
    () =>
      trackedCodeDeliveryRepositories(discovered, {
        manualRepositories,
        excludedRegisteredRepoIds,
        pinnedRepositoryKeys,
      }),
    [
      discovered,
      manualRepositories,
      excludedRegisteredRepoIds,
      pinnedRepositoryKeys,
    ],
  );

  const unread = unreadCodeDeliveryNotifications({ notifications });
  const visible = useMemo(() => {
    const query = search.trim().toLocaleLowerCase();
    return notifications.filter((notification) => {
      if (filter === "unread" && notification.readAt) return false;
      if (filter !== "all" && filter !== "unread" && notification.rule !== filter) {
        return false;
      }
      if (!query) return true;
      return [
        notification.title,
        notification.detail,
        notification.repositoryName,
      ]
        .join(" ")
        .toLocaleLowerCase()
        .includes(query);
    });
  }, [filter, notifications, search]);

  const openNotification = (notification: CodeDeliveryNotification) => {
    useCodeDeliveryStore.getState().markNotificationRead(notification.id);
    if (notification.target.kind === "pull_request") {
      void navigate({ to: "/code/delivery/pull-requests" });
    } else {
      void navigate({ to: "/code/delivery/runs" });
    }
  };

  return (
    <div className="flex size-full min-h-0 flex-col bg-background">
      <header className="shrink-0 border-b border-border-subtle px-5 pt-4">
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div>
            <div className="flex items-center gap-2">
              <h1 className="text-xl font-semibold tracking-tight">Notifications</h1>
              {unread > 0 && (
                <span className="rounded-full bg-primary px-2 py-0.5 text-[11px] font-medium text-primary-foreground">
                  {unread} unread
                </span>
              )}
            </div>
            <p className="mt-0.5 text-sm text-muted-foreground">
              Delivery changes you asked Tidebreak to keep watching.
            </p>
          </div>
          <div className="flex items-center gap-2 text-xs text-muted-foreground">
            {polling ? (
              <span>Checking GitHub…</span>
            ) : lastSuccessfulPollAt ? (
              <span>Checked {relativeTime(lastSuccessfulPollAt)}</span>
            ) : (
              <span>Waiting for first check</span>
            )}
          </div>
        </div>
        <Tabs value={tab} onValueChange={setTab} className="mt-3">
          <TabsList>
            <TabsTrigger value="feed">
              <Bell />
              Feed
            </TabsTrigger>
            <TabsTrigger value="rules">
              <Settings2 />
              Rules
            </TabsTrigger>
          </TabsList>
        </Tabs>
      </header>

      {monitorError && (
        <div className="flex shrink-0 items-start gap-2 border-b border-warning-border bg-warning-background px-5 py-2.5 text-xs text-warning-foreground-muted">
          <CircleAlert className="mt-0.5 size-3.5 shrink-0" />
          <span>Delivery monitoring could not refresh: {monitorError}</span>
        </div>
      )}

      {tab === "feed" ? (
        <>
          <div className="flex shrink-0 flex-wrap items-center gap-2 border-b border-border-subtle px-5 py-3">
            <SearchInput
              size="sm"
              value={search}
              onValueChange={setSearch}
              placeholder="Search notifications…"
              className="min-w-56 flex-1 md:max-w-md"
            />
            <Select value={filter} onValueChange={(value) => setFilter(value as FeedFilter)}>
              <SelectTrigger size="sm" className="w-44">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">All notifications</SelectItem>
                <SelectItem value="unread">Unread</SelectItem>
                <SelectItem value="pull_request_attention">PR attention</SelectItem>
                <SelectItem value="pull_request_ready">PR ready</SelectItem>
                <SelectItem value="run_failure">Run failures</SelectItem>
              </SelectContent>
            </Select>
            <Button
              type="button"
              size="sm"
              variant="outline"
              disabled={unread === 0}
              onClick={() => useCodeDeliveryStore.getState().markAllNotificationsRead()}
            >
              <CheckCheck />
              Mark all read
            </Button>
            <Button
              type="button"
              size="icon-sm"
              variant="ghost-destructive"
              disabled={notifications.length === 0}
              aria-label="Clear notification history"
              onClick={() => useCodeDeliveryStore.getState().clearNotifications()}
            >
              <Trash2 />
            </Button>
          </div>
          <div className="min-h-0 flex-1 overflow-auto">
            {notifications.length === 0 ? (
              <Empty className="min-h-80">
                <EmptyHeader>
                  <EmptyMedia variant="icon">
                    <Bell />
                  </EmptyMedia>
                  <EmptyTitle>No delivery notifications yet</EmptyTitle>
                  <EmptyDescription>
                    New PR attention, merge readiness, and failed runs appear here.
                  </EmptyDescription>
                </EmptyHeader>
              </Empty>
            ) : visible.length === 0 ? (
              <Empty className="min-h-72">
                <EmptyHeader>
                  <EmptyTitle>No notifications match</EmptyTitle>
                  <EmptyDescription>Change the search or feed filter.</EmptyDescription>
                </EmptyHeader>
              </Empty>
            ) : (
              <div role="list" aria-label="Delivery notifications">
                {visible.map((notification) => (
                  <NotificationRow
                    key={notification.id}
                    notification={notification}
                    onOpen={() => openNotification(notification)}
                    onOpenWorkspace={
                      notification.workspaceId
                        ? () => {
                            useCodeDeliveryStore
                              .getState()
                              .markNotificationRead(notification.id);
                            void navigate({
                              to: "/code/w/$workspaceId",
                              params: { workspaceId: notification.workspaceId! },
                            });
                          }
                        : undefined
                    }
                  />
                ))}
              </div>
            )}
          </div>
        </>
      ) : (
        <NotificationRules rules={rules} repositories={repositories} />
      )}
    </div>
  );
}

function NotificationRow({
  notification,
  onOpen,
  onOpenWorkspace,
}: {
  notification: CodeDeliveryNotification;
  onOpen: () => void;
  onOpenWorkspace?: () => void;
}) {
  const icon =
    notification.rule === "run_failure" ? (
      <Workflow className="size-4 text-critical" />
    ) : notification.rule === "pull_request_ready" ? (
      <GitPullRequest className="size-4 text-success" />
    ) : (
      <GitPullRequest className="size-4 text-warning" />
    );
  return (
    <article
      role="listitem"
      className={cn(
        "grid grid-cols-[24px_minmax(0,1fr)_auto] gap-3 border-b border-border-subtle px-5 py-3.5",
        !notification.readAt && "bg-info-background/35",
      )}
    >
      <div className="mt-0.5">{icon}</div>
      <button type="button" className="min-w-0 cursor-pointer text-left" onClick={onOpen}>
        <div className="flex min-w-0 items-center gap-2">
          <h2 className="truncate text-sm font-medium">{notification.title}</h2>
          {!notification.readAt && (
            <span className="size-1.5 shrink-0 rounded-full bg-primary" aria-label="Unread" />
          )}
        </div>
        <p className="mt-0.5 truncate text-xs text-muted-foreground">
          {notification.detail}
        </p>
        <p className="mt-1 text-[11px] text-muted-foreground">
          {relativeTime(notification.occurredAt)}
        </p>
      </button>
      <div className="flex items-center gap-1">
        {onOpenWorkspace && (
          <Button
            type="button"
            size="xs"
            variant="outline"
            onClick={onOpenWorkspace}
          >
            <GitBranch />
            Workspace
          </Button>
        )}
        <Button
          type="button"
          size="icon-xs"
          variant="ghost"
          aria-label="Open on GitHub"
          onClick={() => {
            useCodeDeliveryStore
              .getState()
              .markNotificationRead(notification.id);
            void openInBrowser(notification.url);
          }}
        >
          <ExternalLink />
        </Button>
      </div>
    </article>
  );
}

function NotificationRules({
  rules,
  repositories,
}: {
  rules: CodeDeliveryNotificationRule[];
  repositories: CodeGitHubRepositoryRef[];
}) {
  return (
    <div className="min-h-0 flex-1 overflow-auto px-5 py-5">
      <div className="mx-auto max-w-3xl">
        <div className="mb-4">
          <h2 className="text-base font-semibold">Delivery notification rules</h2>
          <p className="mt-1 text-sm text-muted-foreground">
            Rules run locally. Repository scope, history, and read state stay on
            this device.
          </p>
        </div>
        <div className="flex flex-col rounded-lg border border-border-subtle">
          {rules.map((rule) => (
            <RuleRow key={rule.id} rule={rule} repositories={repositories} />
          ))}
        </div>
      </div>
    </div>
  );
}

function RuleRow({
  rule,
  repositories,
}: {
  rule: CodeDeliveryNotificationRule;
  repositories: CodeGitHubRepositoryRef[];
}) {
  const copy = ruleCopy(rule.id);
  const update = (patch: Partial<Omit<CodeDeliveryNotificationRule, "id">>) =>
    useCodeDeliveryStore.getState().updateNotificationRule(rule.id, patch);
  return (
    <div className="flex flex-wrap items-center gap-4 border-b border-border-subtle px-4 py-3.5 last:border-b-0">
      <Switch
        checked={rule.enabled}
        onCheckedChange={(enabled) => update({ enabled })}
        aria-label={copy.title}
      />
      <div className="min-w-52 flex-1">
        <h3 className="text-sm font-medium">{copy.title}</h3>
        <p className="mt-0.5 text-xs text-muted-foreground">{copy.description}</p>
      </div>
      <label className="flex cursor-pointer items-center gap-2 text-xs">
        <Checkbox
          checked={rule.tidebreakLinkedOnly}
          onCheckedChange={(checked) =>
            update({ tidebreakLinkedOnly: checked === true })
          }
        />
        Linked only
      </label>
      <RuleRepositoryScope
        repositories={repositories}
        selected={rule.repositoryKeys}
        onChange={(repositoryKeys) => update({ repositoryKeys })}
      />
    </div>
  );
}

function RuleRepositoryScope({
  repositories,
  selected,
  onChange,
}: {
  repositories: CodeGitHubRepositoryRef[];
  selected: string[];
  onChange: (selected: string[]) => void;
}) {
  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button type="button" size="sm" variant="outline">
          {selected.length === 0
            ? "All repositories"
            : `${selected.length} repos`}
        </Button>
      </PopoverTrigger>
      <PopoverContent align="end" className="w-72 p-2">
        <button
          type="button"
          className="mb-1 w-full cursor-pointer rounded-md px-2 py-1.5 text-left text-xs hover:bg-muted/40"
          onClick={() => onChange([])}
        >
          All repositories
        </button>
        <div className="max-h-56 overflow-auto">
          {repositories.map((repository) => {
            const key = codeDeliveryRepositoryKey(repository);
            return (
              <label
                key={key}
                className="flex cursor-pointer items-center gap-2 rounded-md px-2 py-1.5 text-xs hover:bg-muted/40"
              >
                <Checkbox
                  checked={selected.includes(key)}
                  onCheckedChange={(checked) =>
                    onChange(toggleValue(selected, key, checked === true))
                  }
                />
                <span className="min-w-0 truncate">{repository.name_with_owner}</span>
              </label>
            );
          })}
        </div>
      </PopoverContent>
    </Popover>
  );
}

function ruleCopy(kind: CodeDeliveryNotificationRuleKind): {
  title: string;
  description: string;
} {
  switch (kind) {
    case "pull_request_attention":
      return {
        title: "Pull requests need attention",
        description: "Changes requested, failed checks, conflicts, or blocked state.",
      };
    case "pull_request_ready":
      return {
        title: "Pull requests are ready to merge",
        description: "Review and checks have reached a merge-ready state.",
      };
    case "run_failure":
      return {
        title: "Runs and deployments fail",
        description: "Failure, timeout, startup failure, or action required.",
      };
  }
}

function toggleValue(values: string[], value: string, enabled: boolean): string[] {
  if (enabled) return values.includes(value) ? values : [...values, value];
  return values.filter((candidate) => candidate !== value);
}

function relativeTime(value: string): string {
  try {
    return formatDistanceToNowStrict(new Date(value), { addSuffix: true });
  } catch {
    return value;
  }
}
