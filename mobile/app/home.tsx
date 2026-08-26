import { useQuery } from "@tanstack/react-query";
import { useRouter } from "expo-router";
import { Pressable, Text, View } from "react-native";
import { Screen, Body, ErrorText } from "../src/components/Screen";
import { parseWorkspaceList, workspaceDisplayName } from "../src/lib/attach";
import { fetchIdentity } from "../src/lib/gateway";
import { fetchRefusingRedirects } from "../src/lib/http";
import { RESOURCE_CONTROL } from "../src/lib/resource";
import { tokenStore } from "../src/session/runtime";
import { useSessionStore } from "../src/session/store";

export default function HomeScreen() {
  const router = useRouter();
  const session = useSessionStore((state) => state.session);
  const setSession = useSessionStore((state) => state.setSession);

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
    queryKey: ["workspaces", session?.machine?.baseUrl],
    enabled: !!session?.machine,
    queryFn: async () => {
      const token = await tokenStore.getAccessToken(session!.machine!.resource);
      const response = await fetchRefusingRedirects(
        `${session!.machine!.baseUrl}/code/workspaces`,
        { headers: { Authorization: `Bearer ${token}` } },
      );
      if (!response.ok) {
        throw new Error(`Workspace list failed (HTTP ${response.status})`);
      }
      return parseWorkspaceList(await response.json());
    },
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

  return (
    <Screen title="Attached">
      <View className="rounded-xl border border-border bg-background p-4 gap-1">
        <Text className="text-xs uppercase tracking-wide text-muted-foreground">
          Signed in
        </Text>
        <Text className="text-base text-foreground">
          {identity?.display_name || identity?.email || identity?.user_id || "…"}
        </Text>
        {identity?.email && identity.display_name ? (
          <Text className="text-sm text-muted-foreground">{identity.email}</Text>
        ) : null}
      </View>
      <View className="rounded-xl border border-border bg-background p-4 gap-1">
        <Text className="text-xs uppercase tracking-wide text-muted-foreground">
          Machine
        </Text>
        <Text className="text-base text-foreground">{session.machine.baseUrl}</Text>
      </View>
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
        {workspaces.map((workspace) => (
          <Text key={workspace.id} className="text-sm text-muted-foreground">
            {workspaceDisplayName(workspace)}
          </Text>
        ))}
      </View>
      <Pressable
        className="rounded-lg bg-primary px-4 py-3"
        onPress={() => router.push("/sessions")}
      >
        <Text className="text-center text-base font-medium text-primary-foreground">
          Sessions
        </Text>
      </Pressable>
      <Pressable
        className="rounded-lg border border-border bg-background px-4 py-3"
        onPress={() => router.push("/settings")}
      >
        <Text className="text-center text-base text-foreground">Settings</Text>
      </Pressable>
    </Screen>
  );
}
