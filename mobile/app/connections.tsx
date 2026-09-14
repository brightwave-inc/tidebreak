import { useRouter } from "expo-router";
import { useState } from "react";
import { Pressable, Text, View } from "react-native";
import { Screen, Body } from "../src/components/Screen";
import {
  connectionDetail,
  connectionLabel,
  type GatewayConnection,
} from "../src/lib/connections";
import { connections } from "../src/session/runtime";
import { useConnectionStore } from "../src/session/store";

/**
 * Connection management: what this phone is paired with, which one is active,
 * and how to leave one.
 *
 * Deliberately plain. The merged app's information architecture is #3314's
 * design pass; this screen exists so the plural store is reachable and
 * testable by hand, not to settle how it should look.
 */
function ConnectionRow({
  connection,
  active,
  onSwitch,
  onRemove,
  busy,
}: {
  connection: GatewayConnection;
  active: boolean;
  onSwitch: () => void;
  onRemove: () => void;
  busy: boolean;
}) {
  return (
    <View className="gap-2 border-t border-border py-3 first:border-t-0">
      <Pressable
        accessibilityRole="button"
        accessibilityLabel={`Switch to ${connectionLabel(connection)}`}
        disabled={active || busy}
        className="flex-row items-center justify-between gap-3"
        onPress={onSwitch}
      >
        <View className="min-w-0 flex-1 gap-0.5">
          <Text className="text-base text-foreground" numberOfLines={1}>
            {connectionLabel(connection)}
          </Text>
          <Text className="text-xs text-muted-foreground" numberOfLines={1}>
            {connectionDetail(connection)}
          </Text>
        </View>
        <Text className="text-sm text-muted-foreground">
          {active ? "Active" : "Switch  ›"}
        </Text>
      </Pressable>
      <Pressable
        accessibilityRole="button"
        accessibilityLabel={`Sign out of ${connectionLabel(connection)}`}
        disabled={busy}
        className="self-start rounded-lg border border-border px-3 py-1.5"
        onPress={onRemove}
      >
        <Text className="text-sm text-critical-foreground">Sign out</Text>
      </Pressable>
    </View>
  );
}

export default function ConnectionsScreen() {
  const router = useRouter();
  const list = useConnectionStore((state) => state.connections);
  const activeId = useConnectionStore((state) => state.activeId);
  const [busy, setBusy] = useState(false);

  async function switchTo(id: string) {
    setBusy(true);
    try {
      await connections.setActive(id);
      const next = connections.active();
      router.replace(next?.machine ? "/home" : "/attach");
    } finally {
      setBusy(false);
    }
  }

  async function remove(id: string) {
    setBusy(true);
    try {
      await connections.remove(id);
      const next = connections.active();
      router.replace(next?.machine ? "/home" : next ? "/attach" : "/");
    } finally {
      setBusy(false);
    }
  }

  return (
    <Screen title="Connections">
      <Body>
        Each connection keeps its own credential. Signing out of one leaves the
        others signed in.
      </Body>
      {list.length === 0 ? (
        <Body>No gateway is paired yet.</Body>
      ) : (
        <View className="rounded-xl border border-border bg-background px-4">
          {list.map((connection) => (
            <ConnectionRow
              key={connection.id}
              connection={connection}
              active={connection.id === activeId}
              busy={busy}
              onSwitch={() => void switchTo(connection.id)}
              onRemove={() => void remove(connection.id)}
            />
          ))}
        </View>
      )}
      <Pressable
        disabled={busy}
        className="rounded-lg bg-primary px-4 py-3"
        onPress={() => router.push("/pair")}
      >
        <Text className="text-center text-base font-medium text-primary-foreground">
          Pair another gateway
        </Text>
      </Pressable>
    </Screen>
  );
}
