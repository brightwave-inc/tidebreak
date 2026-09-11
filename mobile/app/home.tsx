import { useQuery } from "@tanstack/react-query";
import Feather from "@expo/vector-icons/Feather";
import { useIsFocused, useRouter } from "expo-router";
import { useMemo, useState } from "react";
import {
  Pressable,
  RefreshControl,
  ScrollView,
  Text,
  View,
} from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import { SectionLabel } from "../src/components/Controls";
import { Body, ErrorText, Screen } from "../src/components/Screen";
import { listActiveCodeWorkspaces, listCodeApprovals } from "../src/lib/api";
import { pendingApprovals } from "../src/lib/approvals";
import {
  listMobileDeliveryRepositories,
  mobileDeliveryNeedsYouCountLabel,
  mobileDeliveryRepositoryTarget,
  queryMobileDeliveryPullRequests,
} from "../src/lib/deliveryApi";
import { fetchIdentity } from "../src/lib/gateway";
import { RESOURCE_CONTROL } from "../src/lib/resource";
import { attentionSessionCount } from "../src/lib/updates";
import { tokenStore } from "../src/session/runtime";
import { useSessionStore } from "../src/session/store";
import { useMachineClient } from "../src/session/useMachineClient";
import { useHasSnapshot, useListedSessions } from "../src/session/updatesStore";
import { useUpdatesFeed } from "../src/session/useUpdatesFeed";

function CountTile({
  label,
  value,
  warn,
  onPress,
}: {
  label: string;
  value: string;
  warn: boolean;
  onPress: () => void;
}) {
  return (
    <Pressable
      accessibilityRole="button"
      accessibilityLabel={`${label}: ${value}`}
      className={`flex-1 items-center gap-1 rounded-xl border px-2 py-3 ${
        warn
          ? "border-warning-border bg-warning-background"
          : "border-border bg-background"
      }`}
      onPress={onPress}
    >
      <Text
        className={`text-2xl font-semibold ${
          warn ? "text-warning-foreground" : "text-foreground"
        }`}
      >
        {value}
      </Text>
      <Text
        className={`text-xs ${
          warn ? "text-warning-foreground" : "text-muted-foreground"
        }`}
      >
        {label}
      </Text>
    </Pressable>
  );
}

function SectionRow({
  label,
  detail,
  first = false,
  onPress,
}: {
  label: string;
  detail?: string;
  first?: boolean;
  onPress: () => void;
}) {
  return (
    <Pressable
      accessibilityRole="button"
      accessibilityLabel={label}
      className={`flex-row items-center justify-between py-3 ${
        first ? "" : "border-t border-border"
      }`}
      onPress={onPress}
    >
      <Text className="text-base text-foreground">{label}</Text>
      <Text className="text-sm text-muted-foreground">
        {detail ? `${detail}  ›` : "›"}
      </Text>
    </Pressable>
  );
}

export default function HomeScreen() {
  const router = useRouter();
  const isFocused = useIsFocused();
  const session = useSessionStore((state) => state.session);
  const setSession = useSessionStore((state) => state.setSession);
  const client = useMachineClient();
  const { live, refresh } = useUpdatesFeed(client);
  const sessions = useListedSessions();
  const hasSnapshot = useHasSnapshot();
  const [refreshing, setRefreshing] = useState(false);

  const identityQuery = useQuery({
    queryKey: ["identity", session?.gatewayUrl],
    enabled: !!session,
    queryFn: async () => {
      const token = await tokenStore.getAccessToken(RESOURCE_CONTROL);
      const identity = await fetchIdentity(session!.gatewayUrl, token);
      await tokenStore.update({ identity });
      setSession(tokenStore.snapshot());
      return identity;
    },
  });

  const workspacesQuery = useQuery({
    queryKey: ["code-workspaces", session?.machine?.baseUrl],
    enabled: !!client,
    queryFn: () => listActiveCodeWorkspaces(client!),
  });

  const approvalsQuery = useQuery({
    queryKey: ["code-approvals", client],
    enabled: !!client && isFocused,
    queryFn: () => listCodeApprovals(client!),
    refetchInterval: 5_000,
  });

  const repositoriesQuery = useQuery({
    queryKey: ["mobile-delivery-repositories", session?.machine?.baseUrl],
    enabled: !!client && isFocused,
    queryFn: ({ signal }) => listMobileDeliveryRepositories(client!, { signal }),
  });
  const repositorySnapshot = repositoriesQuery.data;
  const repositoryTargets = useMemo(
    () =>
      (repositorySnapshot?.repositories ?? []).map(
        mobileDeliveryRepositoryTarget,
      ),
    [repositorySnapshot?.repositories],
  );
  const repositoryKey = repositoryTargets
    .map((repository) =>
      [repository.host, repository.owner, repository.name].join("/"),
    )
    .join("|");
  const repositoriesAvailable =
    repositorySnapshot?.capability.found === true &&
    repositorySnapshot.capability.authenticated !== false &&
    repositoryTargets.length > 0;
  const viewerLogin = repositorySnapshot?.capability.viewer_login;

  const deliveryQuery = useQuery({
    queryKey: [
      "mobile-delivery-attention",
      session?.machine?.baseUrl,
      repositoryKey,
      viewerLogin ?? "",
    ],
    enabled: !!client && isFocused && repositoriesAvailable,
    queryFn: ({ signal }) =>
      queryMobileDeliveryPullRequests(client!, {
        repositories: repositoryTargets,
        ...(viewerLogin ? { authors: [viewerLogin] } : {}),
        signal,
      }),
  });

  if (!session?.machine) {
    return (
      <Screen title="Not attached">
        <Body>Pair a gateway and attach a machine first.</Body>
        <Pressable
          className="rounded-lg bg-primary px-4 py-3"
          onPress={() => router.replace("/")}
        >
          <Text className="text-center text-primary-foreground">Start over</Text>
        </Pressable>
      </Screen>
    );
  }

  const identity = identityQuery.data ?? session.identity;
  const workspaces = workspacesQuery.data ?? [];
  const machineHost = session.machine.baseUrl.replace(/^https?:\/\//, "");

  const pendingCount = approvalsQuery.data
    ? pendingApprovals(approvalsQuery.data).length
    : null;
  const approvalsValue = approvalsQuery.isError
    ? "—"
    : pendingCount === null
      ? "…"
      : String(pendingCount);

  const needsYouCount = hasSnapshot ? attentionSessionCount(sessions) : null;
  const needsYouValue = needsYouCount === null ? "…" : String(needsYouCount);

  const deliveryUnavailable =
    repositoriesQuery.isError ||
    deliveryQuery.isError ||
    (repositorySnapshot !== undefined &&
      (repositorySnapshot.capability.found !== true ||
        repositorySnapshot.capability.authenticated === false));
  const deliveryValue = deliveryUnavailable
    ? "—"
    : repositorySnapshot === undefined
      ? "…"
      : repositoryTargets.length === 0
        ? "0"
        : deliveryQuery.data
          ? mobileDeliveryNeedsYouCountLabel(deliveryQuery.data)
          : "…";

  const activeSessionCount = hasSnapshot
    ? sessions.filter((digest) => digest.lifecycle !== "ended").length
    : null;

  async function onRefresh() {
    setRefreshing(true);
    refresh();
    await Promise.allSettled([
      identityQuery.refetch(),
      workspacesQuery.refetch(),
      approvalsQuery.refetch(),
      repositoriesQuery.refetch(),
      deliveryQuery.refetch(),
    ]);
    setRefreshing(false);
  }

  return (
    <SafeAreaView className="flex-1 bg-page-background">
      <ScrollView
        contentContainerClassName="gap-4 px-5 py-6"
        refreshControl={
          <RefreshControl
            refreshing={refreshing}
            onRefresh={() => void onRefresh()}
          />
        }
      >
        <Pressable
          accessibilityRole="button"
          accessibilityLabel="Open settings"
          className="flex-row items-center justify-between gap-3 rounded-xl border border-border bg-background p-4"
          onPress={() => router.push("/settings")}
        >
          <View className="flex-1 gap-0.5">
            <Text className="text-base font-medium text-foreground">
              {identity?.display_name || identity?.email || identity?.user_id || "…"}
            </Text>
            <Text className="text-xs text-muted-foreground" numberOfLines={1}>
              {machineHost} · {live ? "Live" : "Reconnecting…"}
            </Text>
          </View>
          <Feather name="settings" size={18} color="#697386" />
        </Pressable>

        <View className="flex-row gap-3">
          <CountTile
            label="Approvals"
            value={approvalsValue}
            warn={pendingCount !== null && pendingCount > 0}
            onPress={() => router.push("/approvals")}
          />
          <CountTile
            label="Needs you"
            value={needsYouValue}
            warn={needsYouCount !== null && needsYouCount > 0}
            onPress={() => router.push("/sessions")}
          />
          <CountTile
            label="Delivery"
            value={deliveryValue}
            warn={!deliveryUnavailable && /^[1-9]/.test(deliveryValue)}
            onPress={() => router.push("/delivery")}
          />
        </View>

        <View className="gap-2">
          <SectionLabel>Work</SectionLabel>
          <View className="rounded-xl border border-border bg-background px-4">
            <SectionRow
              label="Sessions"
              {...(activeSessionCount !== null
                ? { detail: String(activeSessionCount) }
                : {})}
              first
              onPress={() => router.push("/sessions")}
            />
            <SectionRow label="Delivery" onPress={() => router.push("/delivery")} />
            <SectionRow label="Chats" onPress={() => router.push("/chats")} />
          </View>
        </View>

        <View className="gap-2">
          <SectionLabel>Machine</SectionLabel>
          <View className="rounded-xl border border-border bg-background p-4 gap-2">
            <Text className="text-xs uppercase tracking-wide text-muted-foreground">
              Code workspaces
            </Text>
            {workspacesQuery.isError ? (
              <ErrorText>
                {workspacesQuery.error instanceof Error
                  ? workspacesQuery.error.message
                  : "Could not list workspaces."}
              </ErrorText>
            ) : (
              <Text className="text-base text-foreground">
                {workspacesQuery.isLoading
                  ? "Loading…"
                  : `${workspaces.length} workspace${workspaces.length === 1 ? "" : "s"}`}
              </Text>
            )}
            {!workspacesQuery.isLoading &&
            !workspacesQuery.isError &&
            workspaces.length === 0 ? (
              <Text className="text-sm text-muted-foreground">
                No active workspaces are available on this machine.
              </Text>
            ) : null}
            {workspaces.map((workspace) => (
              <Pressable
                key={workspace.id}
                accessibilityRole="button"
                accessibilityLabel={`Start a session in ${workspace.title || workspace.branch_name}`}
                className="gap-1 border-t border-border py-3 first:border-t-0"
                onPress={() =>
                  router.push({
                    pathname: "/workspace/[id]/start",
                    params: { id: workspace.id },
                  })
                }
              >
                <Text className="text-sm font-medium text-foreground">
                  {workspace.title || "Untitled workspace"}
                </Text>
                <Text
                  className="font-mono text-xs text-muted-foreground"
                  numberOfLines={1}
                >
                  {workspace.branch_name}
                </Text>
              </Pressable>
            ))}
            {workspaces.length > 0 ? (
              <Text className="text-xs text-muted-foreground">
                Tap a workspace to start a session.
              </Text>
            ) : null}
          </View>
        </View>
      </ScrollView>
    </SafeAreaView>
  );
}
